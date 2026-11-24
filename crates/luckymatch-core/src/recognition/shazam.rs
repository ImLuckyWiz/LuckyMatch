use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::error::CoreError;
use crate::lyrics::normalize::valid_isrc;
use crate::recognition::algorithm::SignatureGenerator;
use crate::recognition::signature_format::DecodedSignature;

static USER_AGENTS: &[&str] = &[
    "Dalvik/2.1.0 (Linux; U; Android 13; Pixel 7 Build/TQ3A.230805.001)",
    "Dalvik/2.1.0 (Linux; U; Android 12; SM-G991B Build/SP1A.210812.016)",
    "Dalvik/2.1.0 (Linux; U; Android 11; Pixel 5 Build/RQ3A.200805.001.A1)",
    "Dalvik/2.1.0 (Linux; U; Android 13; SM-S918B Build/TP1A.220624.014)",
];

/// Recognized track metadata and playback offset returned by Shazam.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecognitionMatch {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub album_art: Option<String>,
    pub isrc: Option<String>,
    pub apple_id: Option<String>,
    pub shazam_key: Option<String>,
    pub shazam_url: Option<String>,
    pub spotify_track_id: Option<String>,
    /// Playback offset (seconds from start of track) at capture anchor time.
    pub offset_sec: f64,
    /// Playback speed skew reported by Shazam (`0.0` = exact speed).
    pub timeskew: f64,
    /// Frequency skew reported by Shazam.
    pub frequencyskew: f64,
    /// Suggested retry interval in ms when no match was found.
    pub retry_ms: Option<u64>,
}

/// Fingerprint mono 16 kHz `i16` PCM and query Shazam's discovery endpoint.
pub async fn recognize_pcm_16khz(
    client: &reqwest::Client,
    mono_16khz: Vec<i16>,
) -> Result<Option<RecognitionMatch>, CoreError> {
    if mono_16khz.len() < 16_000 {
        return Err(CoreError::InvalidAudio(
            "need at least 1 second (16,000 samples) of mono 16 kHz audio".to_string(),
        ));
    }

    let signature =
        tokio::task::spawn_blocking(move || SignatureGenerator::make_signature_from_buffer(mono_16khz))
            .await
            .map_err(|e| CoreError::Other(format!("fingerprinting task failed: {e}")))?;

    recognize_signature(client, &signature).await
}

/// Send an encoded Shazam signature to Shazam's discovery API.
pub async fn recognize_signature(
    client: &reqwest::Client,
    signature: &DecodedSignature,
) -> Result<Option<RecognitionMatch>, CoreError> {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| CoreError::Other(e.to_string()))?
        .as_millis() as u64;

    let samplems =
        (signature.number_samples as f32 / signature.sample_rate_hz as f32 * 1000.0) as u32;
    let uri = signature
        .encode_to_uri()
        .map_err(|e| CoreError::Other(format!("signature encoding failed: {e}")))?;

    let body = serde_json::json!({
        "geolocation": { "altitude": 300, "latitude": 45, "longitude": 2 },
        "signature": { "samplems": samplems, "timestamp": timestamp_ms, "uri": uri },
        "timestamp": timestamp_ms,
        "timezone": "Europe/Paris",
    });

    let uuid_a = Uuid::new_v4();
    let uuid_b = Uuid::new_v4();
    let uuid_upper = uuid_a.hyphenated().to_string().to_uppercase();
    let uuid_lower = uuid_b.hyphenated().to_string();
    let url = format!(
        "https://amp.shazam.com/discovery/v5/en/US/android/-/tag/{uuid_upper}/{uuid_lower}\
?sync=true&webv3=true&sampling=true&connected=&shazamapiversion=v3&sharehub=true&video=v3"
    );

    let ua = USER_AGENTS[(uuid_a.as_bytes()[0] as usize) % USER_AGENTS.len()];

    let resp = client
        .post(&url)
        .header("Content-Language", "en_US")
        .header(reqwest::header::USER_AGENT, ua)
        .json(&body)
        .send()
        .await
        .map_err(|e| CoreError::Http(format!("Shazam request failed: {e}")))?;

    if resp.status().as_u16() == 429 {
        return Err(CoreError::RateLimited);
    }
    if !resp.status().is_success() {
        return Err(CoreError::Http(format!(
            "Shazam returned status {}",
            resp.status()
        )));
    }

    let json: Value = resp
        .json()
        .await
        .map_err(|e| CoreError::Json(format!("Shazam response parse failed: {e}")))?;

    Ok(parse_shazam_response(&json))
}

/// Parse Shazam discovery JSON response, including `matches[0]` offset and skews.
pub fn parse_shazam_response(json: &Value) -> Option<RecognitionMatch> {
    let track = json.get("track")?;
    let title = str_field(track, "title").filter(|s| !s.trim().is_empty())?;
    let artist = str_field(track, "subtitle").unwrap_or_default();

    let album_art = track
        .pointer("/images/coverarthq")
        .and_then(Value::as_str)
        .or_else(|| track.pointer("/images/coverart").and_then(Value::as_str))
        .map(str::to_string);

    let shazam_key = str_field(track, "key");
    let shazam_url = str_field(track, "url")
        .or_else(|| {
            track
                .pointer("/share/href")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|u| is_real_shazam_url(u));

    let album = track
        .get("sections")
        .and_then(Value::as_array)
        .and_then(|sections| {
            sections
                .iter()
                .find(|s| s.get("type").and_then(Value::as_str) == Some("SONG"))
        })
        .and_then(|song| song.get("metadata"))
        .and_then(Value::as_array)
        .and_then(|md| {
            md.iter()
                .find(|m| m.get("title").and_then(Value::as_str) == Some("Album"))
        })
        .and_then(|m| m.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let isrc = str_field(track, "isrc").and_then(|code| valid_isrc(&code));

    let apple_id = track
        .pointer("/hub/actions")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions
                .iter()
                .find(|a| a.get("type").and_then(Value::as_str) == Some("applemusicplay"))
        })
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str)
        .and_then(valid_apple_id);

    let spotify_track_id = extract_spotify_track_id(track);

    let first_match = json
        .get("matches")
        .and_then(Value::as_array)
        .and_then(|m| m.first());

    let offset_sec = first_match
        .and_then(|m| m.get("offset"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let timeskew = first_match
        .and_then(|m| m.get("timeskew"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let frequencyskew = first_match
        .and_then(|m| m.get("frequencyskew"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let retry_ms = json.get("retryms").and_then(Value::as_u64);

    Some(RecognitionMatch {
        title,
        artist,
        album,
        album_art,
        isrc,
        apple_id,
        shazam_key,
        shazam_url,
        spotify_track_id,
        offset_sec,
        timeskew,
        frequencyskew,
        retry_ms,
    })
}

fn extract_spotify_track_id(track: &Value) -> Option<String> {
    let providers = track.pointer("/hub/providers")?.as_array()?;
    for provider in providers {
        let Some(actions) = provider.get("actions").and_then(Value::as_array) else {
            continue;
        };
        for action in actions {
            let Some(uri) = action.get("uri").and_then(Value::as_str) else {
                continue;
            };
            if let Some(id) = parse_spotify_track_url(uri) {
                return Some(id);
            }
        }
    }
    None
}

fn parse_spotify_track_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("open.spotify.com") {
        return None;
    }
    let mut parts = url.path_segments()?;
    let (Some("track"), Some(id), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    (id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_string())
}

fn valid_apple_id(raw: &str) -> Option<String> {
    let id = raw.trim();
    (!id.is_empty() && id.len() <= 12 && id.bytes().all(|b| b.is_ascii_digit()))
        .then(|| id.to_string())
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn is_real_shazam_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed
            .host_str()
            .is_some_and(|h| h == "www.shazam.com" || h == "shazam.com")
        && !url.contains("unsupported.shazam")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_track_and_match_offset_and_skews() {
        let payload = json!({
            "matches": [
                {
                    "id": "123",
                    "offset": 42.375,
                    "timeskew": 0.00012,
                    "frequencyskew": -0.00004
                }
            ],
            "retryms": 5000,
            "track": {
                "key": "10330538",
                "title": "Tu Nombre",
                "subtitle": "Antonio Machín",
                "isrc": "qm6mz1422245",
                "url": "https://www.shazam.com/track/10330538/tu-nombre",
                "images": {
                    "coverarthq": "https://example.com/art.jpg"
                },
                "hub": {
                    "actions": [
                        { "type": "applemusicplay", "id": "950742158" }
                    ],
                    "providers": [
                        {
                            "actions": [
                                { "uri": "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT" }
                            ]
                        }
                    ]
                },
                "sections": [
                    {
                        "type": "SONG",
                        "metadata": [
                            { "title": "Album", "text": "Antonio Machin" }
                        ]
                    }
                ]
            }
        });

        let m = parse_shazam_response(&payload).expect("expected match");
        assert_eq!(m.title, "Tu Nombre");
        assert_eq!(m.artist, "Antonio Machín");
        assert_eq!(m.album.as_deref(), Some("Antonio Machin"));
        assert_eq!(m.isrc.as_deref(), Some("QM6MZ1422245"));
        assert_eq!(m.apple_id.as_deref(), Some("950742158"));
        assert_eq!(m.shazam_key.as_deref(), Some("10330538"));
        assert_eq!(
            m.spotify_track_id.as_deref(),
            Some("4cOdK2wGLETKBW3PvgPWqT")
        );
        assert!((m.offset_sec - 42.375).abs() < 1e-9);
        assert!((m.timeskew - 0.00012).abs() < 1e-9);
        assert!((m.frequencyskew + 0.00004).abs() < 1e-9);
        assert_eq!(m.retry_ms, Some(5000));
    }

    #[test]
    fn no_match_returns_none() {
        let payload = json!({ "matches": [], "retryms": 6000 });
        assert!(parse_shazam_response(&payload).is_none());
    }
}
