use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::CoreError;
use crate::lyrics::normalize::{clean_title, primary_artist};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpotifyTrackInfo {
    pub track_id: String,
    pub name: String,
    pub artist: String,
    pub duration_ms: u64,
    pub album_art: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Resolves Spotify Track IDs and track durations using Spotify Client Credentials.
#[derive(Clone)]
pub struct SpotifyResolver {
    client: reqwest::Client,
    client_id: Option<String>,
    client_secret: Option<String>,
    token: Arc<RwLock<Option<CachedToken>>>,
}

impl SpotifyResolver {
    pub fn new(
        client: reqwest::Client,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self {
            client,
            client_id,
            client_secret,
            token: Arc::new(RwLock::new(None)),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.client_id.is_some() && self.client_secret.is_some()
    }

    /// Resolve a Spotify track by ISRC first, falling back to cleaned title + primary artist.
    /// Returns `Ok(None)` cleanly when Spotify credentials are not configured.
    pub async fn resolve(
        &self,
        isrc: Option<&str>,
        title: &str,
        artist: &str,
    ) -> Result<Option<SpotifyTrackInfo>, CoreError> {
        if !self.is_configured() {
            return Ok(None);
        }

        let token = self.get_token().await?;

        if let Some(code) = isrc.map(str::trim).filter(|s| !s.is_empty()) {
            let query = format!("isrc:{code}");
            if let Some(hit) = self.search_tracks(&token, &query, 1).await? {
                if let Some(first) = hit.into_iter().next() {
                    return Ok(Some(first));
                }
            }
        }

        let cleaned = clean_title(title);
        let primary = primary_artist(artist);
        let search_title = if cleaned.is_empty() { title } else { &cleaned };
        let search_artist = if primary.is_empty() { artist } else { &primary };
        let text_query = format!("{search_title} {search_artist}");
        if text_query.trim().is_empty() {
            return Ok(None);
        }

        let candidates = self.search_tracks(&token, &text_query, 5).await?;
        let Some(items) = candidates else {
            return Ok(None);
        };

        Ok(pick_best_candidate(items, search_title, search_artist))
    }

    async fn get_token(&self) -> Result<String, CoreError> {
        {
            let guard = self.token.read().await;
            if let Some(tok) = guard.as_ref() {
                if Instant::now() < tok.expires_at {
                    return Ok(tok.access_token.clone());
                }
            }
        }

        let (Some(id), Some(secret)) = (&self.client_id, &self.client_secret) else {
            return Err(CoreError::SpotifyAuth(
                "missing Spotify client_id or client_secret".to_string(),
            ));
        };

        let basic = STANDARD.encode(format!("{id}:{secret}"));
        let resp = self
            .client
            .post("https://accounts.spotify.com/api/token")
            .header(reqwest::header::AUTHORIZATION, format!("Basic {basic}"))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(|e| CoreError::SpotifyAuth(format!("token request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(CoreError::SpotifyAuth(format!(
                "token endpoint returned HTTP {}",
                resp.status()
            )));
        }

        let parsed: TokenResponse = resp
            .json()
            .await
            .map_err(|e| CoreError::SpotifyAuth(format!("token JSON parse failed: {e}")))?;

        let ttl = Duration::from_secs(parsed.expires_in.saturating_sub(60).max(30));
        let token = parsed.access_token.clone();
        let mut guard = self.token.write().await;
        *guard = Some(CachedToken {
            access_token: parsed.access_token,
            expires_at: Instant::now() + ttl,
        });

        Ok(token)
    }

    async fn search_tracks(
        &self,
        token: &str,
        query: &str,
        limit: u8,
    ) -> Result<Option<Vec<SpotifyTrackInfo>>, CoreError> {
        let encoded = urlencoding::encode(query);
        let url = format!(
            "https://api.spotify.com/v1/search?q={encoded}&type=track&limit={limit}"
        );

        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| CoreError::Http(format!("Spotify search failed: {e}")))?;

        if resp.status().as_u16() == 401 {
            // Token expired early; clear cache once.
            *self.token.write().await = None;
            return Err(CoreError::SpotifyAuth("Spotify token rejected (401)".into()));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Http(format!(
                "Spotify search returned HTTP {}",
                resp.status()
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| CoreError::Json(format!("Spotify search parse failed: {e}")))?;

        let Some(items) = json.pointer("/tracks/items").and_then(Value::as_array) else {
            return Ok(None);
        };

        let tracks = items.iter().filter_map(parse_spotify_track).collect();
        Ok(Some(tracks))
    }
}

fn parse_spotify_track(item: &Value) -> Option<SpotifyTrackInfo> {
    let track_id = item.get("id")?.as_str()?.to_string();
    let name = item.get("name")?.as_str()?.to_string();
    let duration_ms = item.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
    let artist = item
        .get("artists")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let album_art = item
        .pointer("/album/images/0/url")
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(SpotifyTrackInfo {
        track_id,
        name,
        artist,
        duration_ms,
        album_art,
    })
}

fn normalize_cmp(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn pick_best_candidate(
    items: Vec<SpotifyTrackInfo>,
    want_title: &str,
    want_artist: &str,
) -> Option<SpotifyTrackInfo> {
    let wt = normalize_cmp(want_title);
    let wa = normalize_cmp(want_artist);

    if let Some(exact) = items.iter().find(|it| {
        normalize_cmp(&clean_title(&it.name)) == wt
            && normalize_cmp(&primary_artist(&it.artist)) == wa
    }) {
        return Some(exact.clone());
    }

    items
        .into_iter()
        .find(|it| normalize_cmp(&primary_artist(&it.artist)) == wa)
}
