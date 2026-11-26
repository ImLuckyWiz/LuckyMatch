use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::config::LuckyMatchConfig;
use crate::lyrics::cache::{cache_key_for_query, check_custom_lyrics, load_cache, save_cache};
use crate::lyrics::lrc::parse_lrc;
use crate::lyrics::model::{
    extract_spicy_duration_ms, LyricsDocument, LyricsKind, LyricsResult, LyricsSource,
};
use crate::lyrics::normalize::{clean_title, primary_artist};
use crate::lyrics::ttml::{parse_ttml, parse_ttml_duration_ms};
use crate::lyrics::yrc::parse_yrc;

const FAST_TIMEOUT: Duration = Duration::from_millis(3500);
const SPICY_TIMEOUT: Duration = Duration::from_secs(35);
const SPICY_HEAD_START: Duration = Duration::from_millis(2200);

#[derive(Debug, Clone)]
pub struct LyricsQuery {
    pub title: String,
    pub artist: String,
    pub spotify_track_id: Option<String>,
    pub isrc: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Run the 6-tier lyrics priority waterfall and stream results as higher-quality tiers arrive.
pub fn fetch_lyrics_streaming(
    client: reqwest::Client,
    config: LuckyMatchConfig,
    query: LyricsQuery,
) -> mpsc::UnboundedReceiver<LyricsResult> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        run_waterfall(client, config, query, tx).await;
    });

    rx
}

/// Run the waterfall and return the highest-quality lyrics result available.
pub async fn fetch_lyrics(
    client: &reqwest::Client,
    config: &LuckyMatchConfig,
    query: &LyricsQuery,
) -> Option<LyricsResult> {
    let mut rx = fetch_lyrics_streaming(client.clone(), config.clone(), query.clone());
    let mut best: Option<LyricsResult> = None;
    while let Some(res) = rx.recv().await {
        let score = res.quality_score();
        let is_better = best.as_ref().is_none_or(|b| score > b.quality_score());
        if is_better {
            best = Some(res);
        }
        if score >= 85 {
            break;
        }
    }
    best
}

async fn run_waterfall(
    client: reqwest::Client,
    config: LuckyMatchConfig,
    query: LyricsQuery,
    tx: mpsc::UnboundedSender<LyricsResult>,
) {
    // 1. Custom local overrides
    if let Some(custom_dir) = &config.custom_lyrics_dir {
        if let Some(doc) = check_custom_lyrics(custom_dir, &query) {
            let _ = tx.send(LyricsResult {
                document: doc,
                source: LyricsSource::Custom,
                duration_ms: None,
            });
            return;
        }
    }

    let cache_key = cache_key_for_query(&query);
    let mut best_score: u8 = 0;

    // 2. Local disk cache
    if let Some(cache_dir) = &config.cache_dir {
        if let Some(cached) = load_cache(cache_dir, &cache_key) {
            let res = LyricsResult {
                document: cached,
                source: LyricsSource::Cache,
                duration_ms: None,
            };
            let score = res.quality_score();
            best_score = best_score.max(score);
            let _ = tx.send(res);
            if score >= 85 {
                return;
            }
        }
    }

    // 3. Spicy Lyrics Official API (concurrent with 2.2s head start)
    let mut spicy_handle = match (
        query.spotify_track_id.as_deref(),
        config.spicy_lyrics_api_key.as_deref(),
    ) {
        (Some(track_id), Some(api_key)) if !track_id.trim().is_empty() => {
            let client_c = client.clone();
            let tid = track_id.to_string();
            let key = api_key.to_string();
            Some(tokio::spawn(async move {
                fetch_from_spicy_lyrics(&client_c, &tid, &key).await
            }))
        }
        _ => None,
    };

    if let Some(handle) = spicy_handle.as_mut() {
        tokio::select! {
            res = handle => {
                spicy_handle = None;
                if let Ok(Some((doc, duration_ms))) = res {
                    let result = LyricsResult {
                        document: doc,
                        source: LyricsSource::SpicyLyrics,
                        duration_ms,
                    };
                    let score = result.quality_score();
                    if score > best_score {
                        if let Some(cache_dir) = &config.cache_dir {
                            save_cache(cache_dir, &cache_key, &result.document);
                        }
                        best_score = score;
                        let _ = tx.send(result);
                    }
                    if best_score >= 90 {
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(SPICY_HEAD_START) => {}
        }
    }

    // 4. AMLL TTML DB
    if let Some(track_id) = query
        .spotify_track_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some((doc, duration_ms)) = fetch_from_amll(&client, track_id).await {
            let result = LyricsResult {
                document: doc,
                source: LyricsSource::Amll,
                duration_ms,
            };
            let score = result.quality_score();
            if score > best_score {
                if let Some(cache_dir) = &config.cache_dir {
                    save_cache(cache_dir, &cache_key, &result.document);
                }
                best_score = score;
                let _ = tx.send(result);
            }
            if best_score >= 85 {
                return;
            }
        }
    }

    // 5. NetEase Cloud Music (YRC syllable or LRC line) — never cancels pending Spicy Lyrics!
    if let Some((doc, duration_ms)) = fetch_from_netease(&client, &query).await {
        let result = LyricsResult {
            document: doc,
            source: LyricsSource::NetEase,
            duration_ms,
        };
        let score = result.quality_score();
        if score > best_score {
            if result.document.kind == LyricsKind::Syllable {
                if let Some(cache_dir) = &config.cache_dir {
                    save_cache(cache_dir, &cache_key, &result.document);
                }
            }
            best_score = score;
            let _ = tx.send(result);
        }
        // Only skip LRCLIB; still let Spicy Lyrics upgrade if in flight.
    }

    // 6. LRCLIB fallback (only if nothing better has arrived yet)
    if best_score == 0 {
        if let Some((doc, duration_ms)) = fetch_from_lrclib(&client, &query).await {
            let result = LyricsResult {
                document: doc,
                source: LyricsSource::LrcLib,
                duration_ms,
            };
            let score = result.quality_score();
            if score > best_score {
                if let Some(cache_dir) = &config.cache_dir {
                    save_cache(cache_dir, &cache_key, &result.document);
                }
                best_score = score;
                let _ = tx.send(result);
            }
        }
    }

    // Wait for slow Spicy Lyrics task if still running so it can upgrade NetEase/LRCLIB.
    if best_score < 90 {
        if let Some(handle) = spicy_handle {
            if let Ok(Some((doc, duration_ms))) = handle.await {
                let result = LyricsResult {
                    document: doc,
                    source: LyricsSource::SpicyLyrics,
                    duration_ms,
                };
                if result.quality_score() > best_score {
                    if let Some(cache_dir) = &config.cache_dir {
                        save_cache(cache_dir, &cache_key, &result.document);
                    }
                    let _ = tx.send(result);
                }
            }
        }
    }
}

async fn fetch_from_spicy_lyrics(
    client: &reqwest::Client,
    track_id: &str,
    api_key: &str,
) -> Option<(LyricsDocument, Option<u64>)> {
    let url = format!(
        "https://api.spicylyrics.org/v1/lyrics/{}",
        urlencoding::encode(track_id)
    );
    let resp = client
        .get(&url)
        .timeout(SPICY_TIMEOUT)
        .bearer_auth(api_key)
        .header(reqwest::header::USER_AGENT, "LuckyMatch/0.1")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: Value = resp.json().await.ok()?;
    let body = json.get("Body")?;
    let doc = LyricsDocument::from_spicy_or_cache_json(body)?;
    Some((doc, extract_spicy_duration_ms(body)))
}

async fn fetch_from_amll(
    client: &reqwest::Client,
    track_id: &str,
) -> Option<(LyricsDocument, Option<u64>)> {
    let url = format!(
        "https://raw.githubusercontent.com/amll-dev/amll-ttml-db/main/spotify-lyrics/{}.ttml",
        urlencoding::encode(track_id)
    );
    let resp = client.get(&url).timeout(FAST_TIMEOUT).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    let doc = parse_ttml(&text)?;
    Some((doc, parse_ttml_duration_ms(&text)))
}

async fn fetch_from_netease(
    client: &reqwest::Client,
    query: &LyricsQuery,
) -> Option<(LyricsDocument, Option<u64>)> {
    let title = clean_title(&query.title);
    let artist = primary_artist(&query.artist);
    let search_q = format!("{title} {artist}");
    if search_q.trim().is_empty() {
        return None;
    }

    let search_url = format!(
        "https://music.163.com/api/cloudsearch/pc?type=1&limit=5&s={}",
        urlencoding::encode(&search_q)
    );
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

    let search_resp = client
        .get(&search_url)
        .timeout(FAST_TIMEOUT)
        .header(reqwest::header::USER_AGENT, ua)
        .send()
        .await
        .ok()?;
    if !search_resp.status().is_success() {
        return None;
    }

    let search_json: Value = search_resp.json().await.ok()?;
    let songs = search_json.pointer("/result/songs")?.as_array()?;
    if songs.is_empty() {
        return None;
    }

    let candidate = if let Some(dur_ms) = query.duration_ms.filter(|&d| d > 0) {
        songs
            .iter()
            .find(|s| {
                s.get("dt")
                    .and_then(Value::as_u64)
                    .is_some_and(|dt| dt.abs_diff(dur_ms) <= 4500)
            })
            .unwrap_or(&songs[0])
    } else {
        &songs[0]
    };

    let dt_ms = candidate
        .get("dt")
        .and_then(Value::as_u64)
        .filter(|&d| d > 0);
    let song_id = candidate.get("id")?.as_u64()?;
    let lyric_url =
        format!("https://music.163.com/api/song/lyric?id={song_id}&lv=-1&tv=-1&rv=-1&yv=-1");
    let lyric_resp = client
        .get(&lyric_url)
        .timeout(FAST_TIMEOUT)
        .header(reqwest::header::USER_AGENT, ua)
        .send()
        .await
        .ok()?;
    if !lyric_resp.status().is_success() {
        return None;
    }

    let lyric_json: Value = lyric_resp.json().await.ok()?;
    if let Some(yrc) = lyric_json.pointer("/yrc/lyric").and_then(Value::as_str) {
        if let Some(doc) = parse_yrc(yrc) {
            return Some((doc, dt_ms));
        }
    }
    if let Some(lrc) = lyric_json.pointer("/lrc/lyric").and_then(Value::as_str) {
        if let Some(doc) = parse_lrc(lrc, query.duration_ms.or(dt_ms)) {
            return Some((doc, dt_ms));
        }
    }

    None
}

async fn fetch_from_lrclib(
    client: &reqwest::Client,
    query: &LyricsQuery,
) -> Option<(LyricsDocument, Option<u64>)> {
    let title = clean_title(&query.title);
    let artist = primary_artist(&query.artist);
    let use_title = if title.is_empty() {
        query.title.as_str()
    } else {
        title.as_str()
    };
    let use_artist = if artist.is_empty() {
        query.artist.as_str()
    } else {
        artist.as_str()
    };

    let mut url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}",
        urlencoding::encode(use_title),
        urlencoding::encode(use_artist)
    );
    if let Some(dur_ms) = query.duration_ms.filter(|&d| d > 0) {
        let secs = (dur_ms as f64 / 1000.0).round() as u64;
        url.push_str(&format!("&duration={secs}"));
    }

    let resp = client
        .get(&url)
        .timeout(FAST_TIMEOUT)
        .header(reqwest::header::USER_AGENT, "LuckyMatch/0.1")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let json: Value = resp.json().await.ok()?;
    let dur_ms = json
        .get("duration")
        .and_then(Value::as_f64)
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|s| (s * 1000.0).round() as u64);
    let synced = json.get("syncedLyrics").and_then(Value::as_str)?;
    let doc = parse_lrc(synced, query.duration_ms.or(dur_ms))?;
    Some((doc, dur_ms))
}
