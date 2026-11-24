use thiserror::Error;

/// Errors returned by `luckymatch-core`.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid audio input: {0}")]
    InvalidAudio(String),

    #[error("Shazam is rate-limiting requests; try again shortly")]
    RateLimited,

    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("JSON parse error: {0}")]
    Json(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Spotify authentication failed: {0}")]
    SpotifyAuth(String),

    #[error("XML/TTML parse error: {0}")]
    Xml(String),

    #[error("{0}")]
    Other(String),
}
