use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Runtime configuration for `LuckyMatchEngine`.
///
/// Storage paths (`cache_dir`, `custom_lyrics_dir`) are injected by the host
/// application so `luckymatch-core` stays portable across Windows and Android.
#[derive(Debug, Clone, Default)]
pub struct LuckyMatchConfig {
    pub spotify_client_id: Option<String>,
    pub spotify_client_secret: Option<String>,
    pub spicy_lyrics_api_key: Option<String>,
    pub cache_dir: Option<PathBuf>,
    pub custom_lyrics_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfigFile {
    #[serde(rename = "spotifyClientId", alias = "clientId")]
    spotify_client_id: Option<String>,
    #[serde(rename = "spotifyClientSecret", alias = "clientSecret")]
    spotify_client_secret: Option<String>,
    #[serde(rename = "spicyLyricsApiKey")]
    spicy_lyrics_api_key: Option<String>,
    #[serde(rename = "cacheDir")]
    cache_dir: Option<String>,
    #[serde(rename = "customLyricsDir")]
    custom_lyrics_dir: Option<String>,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn env_non_empty(key: &str) -> Option<String> {
    non_empty(std::env::var(key).ok())
}

impl LuckyMatchConfig {
    /// Load configuration from `config_path` (or `./config.json` if it exists),
    /// then override any non-empty credentials from environment variables:
    /// `SPOTIFY_CLIENT_ID`, `SPOTIFY_CLIENT_SECRET`, `SPICY_LYRICS_API_KEY`.
    pub fn load(config_path: Option<&Path>) -> Self {
        let mut cfg = Self::default();

        let candidate_path = config_path
            .map(PathBuf::from)
            .or_else(|| {
                let default = PathBuf::from("config.json");
                default.exists().then_some(default)
            });

        if let Some(path) = candidate_path {
            match std::fs::read_to_string(&path) {
                Ok(raw) => {
                    let clean = raw.trim_start_matches('\u{FEFF}');
                    match serde_json::from_str::<RawConfigFile>(clean) {
                        Ok(parsed) => {
                            cfg.spotify_client_id = non_empty(parsed.spotify_client_id);
                            cfg.spotify_client_secret = non_empty(parsed.spotify_client_secret);
                            cfg.spicy_lyrics_api_key = non_empty(parsed.spicy_lyrics_api_key);
                            cfg.cache_dir = non_empty(parsed.cache_dir).map(PathBuf::from);
                            cfg.custom_lyrics_dir =
                                non_empty(parsed.custom_lyrics_dir).map(PathBuf::from);
                        }
                        Err(err) => {
                            tracing::warn!(
                                "Failed to parse config file {}: {err}",
                                path.display()
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("Failed to read config file {}: {err}", path.display());
                }
            }
        }

        if let Some(v) = env_non_empty("SPOTIFY_CLIENT_ID") {
            cfg.spotify_client_id = Some(v);
        }
        if let Some(v) = env_non_empty("SPOTIFY_CLIENT_SECRET") {
            cfg.spotify_client_secret = Some(v);
        }
        if let Some(v) = env_non_empty("SPICY_LYRICS_API_KEY") {
            cfg.spicy_lyrics_api_key = Some(v);
        }

        cfg
    }

    pub fn has_spotify_credentials(&self) -> bool {
        self.spotify_client_id.is_some() && self.spotify_client_secret.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_config_even_with_utf8_bom() {
        let dir = std::env::temp_dir().join("luckymatch_bom_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config_bom.json");
        let content = "\u{FEFF}{\"spotifyClientId\":\"id123\",\"spotifyClientSecret\":\"sec456\",\"spicyLyricsApiKey\":\"sp789\"}";
        std::fs::write(&path, content).unwrap();

        let cfg = LuckyMatchConfig::load(Some(&path));
        assert_eq!(cfg.spotify_client_id.as_deref(), Some("id123"));
        assert_eq!(cfg.spotify_client_secret.as_deref(), Some("sec456"));
        assert_eq!(cfg.spicy_lyrics_api_key.as_deref(), Some("sp789"));

        let _ = std::fs::remove_file(&path);
    }
}
