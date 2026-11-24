use std::path::PathBuf;
use std::time::Instant;

use luckymatch_core::lyrics::{lrc::parse_lrc, ttml::parse_ttml, yrc::parse_yrc};
use luckymatch_core::{
    evaluate_lyrics, CoreError, LuckyMatchConfig, LuckyMatchEngine, LyricsDocument, LyricsKind,
    LyricsLine, LyricsQuery, LyricsResult, LyricsSource, RecognitionMatch, SpotifyTrackInfo,
    Syllable, SyllableProgress, SyncSnapshot, VocalTrack,
};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("Invalid audio: {detail}")]
    InvalidAudio { detail: String },
    #[error("Rate limited by Shazam")]
    RateLimited,
    #[error("Network error: {detail}")]
    Network { detail: String },
    #[error("Internal error: {detail}")]
    Internal { detail: String },
}

impl From<CoreError> for FfiError {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::InvalidAudio(detail) => FfiError::InvalidAudio { detail },
            CoreError::RateLimited => FfiError::RateLimited,
            CoreError::Http(detail) | CoreError::SpotifyAuth(detail) => {
                FfiError::Network { detail }
            }
            other => FfiError::Internal {
                detail: other.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiConfig {
    pub spotify_client_id: Option<String>,
    pub spotify_client_secret: Option<String>,
    pub spicy_lyrics_api_key: Option<String>,
    pub cache_dir: Option<String>,
    pub custom_lyrics_dir: Option<String>,
}

impl From<FfiConfig> for LuckyMatchConfig {
    fn from(c: FfiConfig) -> Self {
        Self {
            spotify_client_id: c.spotify_client_id.filter(|s| !s.trim().is_empty()),
            spotify_client_secret: c.spotify_client_secret.filter(|s| !s.trim().is_empty()),
            spicy_lyrics_api_key: c.spicy_lyrics_api_key.filter(|s| !s.trim().is_empty()),
            cache_dir: c.cache_dir.filter(|s| !s.trim().is_empty()).map(PathBuf::from),
            custom_lyrics_dir: c
                .custom_lyrics_dir
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRecognizedTrack {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub album_art: Option<String>,
    pub isrc: Option<String>,
    pub apple_id: Option<String>,
    pub spotify_track_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub offset_sec: f64,
    pub timeskew: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLyricsKind {
    Syllable,
    Line,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLyricsSource {
    Custom,
    Cache,
    SpicyLyrics,
    Amll,
    NetEase,
    LrcLib,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyllable {
    pub text: String,
    pub transliterated_text: Option<String>,
    pub start_time: f64,
    pub end_time: f64,
    pub is_part_of_word: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiVocalTrack {
    pub start_time: f64,
    pub end_time: f64,
    pub syllables: Vec<FfiSyllable>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLyricsLine {
    pub start_time: f64,
    pub end_time: f64,
    pub opposite_aligned: bool,
    pub lead: FfiVocalTrack,
    pub background: Vec<FfiVocalTrack>,
    pub text: String,
    pub transliterated_text: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLyricsDocument {
    pub kind: FfiLyricsKind,
    pub lines: Vec<FfiLyricsLine>,
    pub song_writers: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLyricsResult {
    pub document: FfiLyricsDocument,
    pub source: FfiLyricsSource,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyllableProgress {
    pub text: String,
    pub progress: f64,
    pub is_part_of_word: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncSnapshot {
    pub position_sec: f64,
    pub active_index: Option<u32>,
    pub focus_index: Option<u32>,
    pub in_instrumental_gap: bool,
    pub lead_syllables: Vec<FfiSyllableProgress>,
    pub background_lines: Vec<Vec<FfiSyllableProgress>>,
}

#[derive(uniffi::Object)]
pub struct LuckyMatchClient {
    engine: LuckyMatchEngine,
}

#[uniffi::export(async_runtime = "tokio")]
impl LuckyMatchClient {
    #[uniffi::constructor]
    pub fn new(config: FfiConfig) -> Result<Self, FfiError> {
        let engine = LuckyMatchEngine::new(config.into())?;
        Ok(Self { engine })
    }

    /// Recognize a song from interleaved `i16` PCM frames captured on the host platform.
    ///
    /// On Android, record `captureStartMs = SystemClock.elapsedRealtime()` right when audio
    /// capture starts; compute live playback position as:
    /// `offsetSec + ((nowMs - captureStartMs) / 1000.0) * (1.0 + timeskew) + manualOffsetSec`.
    pub async fn recognize_pcm_i16(
        &self,
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Option<FfiRecognizedTrack>, FfiError> {
        let res = self
            .engine
            .recognize_pcm_i16(&samples, sample_rate, channels, Instant::now())
            .await?;

        Ok(res.map(|track| {
            let RecognitionMatch {
                title,
                artist,
                album,
                album_art,
                isrc,
                apple_id,
                spotify_track_id,
                offset_sec,
                timeskew,
                ..
            } = track.recognition;
            let duration_ms = track
                .spotify
                .as_ref()
                .map(|sp: &SpotifyTrackInfo| sp.duration_ms)
                .filter(|&d| d > 0);
            FfiRecognizedTrack {
                title,
                artist,
                album,
                album_art,
                isrc,
                apple_id,
                spotify_track_id,
                duration_ms,
                offset_sec,
                timeskew,
            }
        }))
    }

    pub async fn fetch_lyrics(
        &self,
        title: String,
        artist: String,
        spotify_track_id: Option<String>,
        isrc: Option<String>,
        duration_ms: Option<u64>,
    ) -> Option<FfiLyricsResult> {
        let query = LyricsQuery {
            title,
            artist,
            spotify_track_id,
            isrc,
            duration_ms,
        };
        self.engine.fetch_lyrics(&query).await.map(Into::into)
    }

    pub fn evaluate_sync(
        &self,
        doc: &FfiLyricsDocument,
        position_sec: f64,
        hold_gap_sec: f64,
    ) -> FfiSyncSnapshot {
        let core_doc: LyricsDocument = doc.clone().into();
        evaluate_lyrics(&core_doc, position_sec, hold_gap_sec).into()
    }

    pub fn parse_lyrics(
        &self,
        format: String,
        content: String,
        duration_ms: Option<u64>,
    ) -> Option<FfiLyricsDocument> {
        let doc = match format.to_ascii_lowercase().as_str() {
            "ttml" => parse_ttml(&content),
            "yrc" => parse_yrc(&content),
            "lrc" => parse_lrc(&content, duration_ms),
            "json" => serde_json::from_str::<serde_json::Value>(&content)
                .ok()
                .and_then(|v| LyricsDocument::from_spicy_or_cache_json(&v)),
            _ => None,
        }?;
        Some(doc.into())
    }
}

impl From<LyricsKind> for FfiLyricsKind {
    fn from(k: LyricsKind) -> Self {
        match k {
            LyricsKind::Syllable => FfiLyricsKind::Syllable,
            LyricsKind::Line => FfiLyricsKind::Line,
            LyricsKind::Static => FfiLyricsKind::Static,
        }
    }
}

impl From<FfiLyricsKind> for LyricsKind {
    fn from(k: FfiLyricsKind) -> Self {
        match k {
            FfiLyricsKind::Syllable => LyricsKind::Syllable,
            FfiLyricsKind::Line => LyricsKind::Line,
            FfiLyricsKind::Static => LyricsKind::Static,
        }
    }
}

impl From<LyricsSource> for FfiLyricsSource {
    fn from(s: LyricsSource) -> Self {
        match s {
            LyricsSource::Custom => FfiLyricsSource::Custom,
            LyricsSource::Cache => FfiLyricsSource::Cache,
            LyricsSource::SpicyLyrics => FfiLyricsSource::SpicyLyrics,
            LyricsSource::Amll => FfiLyricsSource::Amll,
            LyricsSource::NetEase => FfiLyricsSource::NetEase,
            LyricsSource::LrcLib => FfiLyricsSource::LrcLib,
        }
    }
}

impl From<Syllable> for FfiSyllable {
    fn from(s: Syllable) -> Self {
        Self {
            text: s.text,
            transliterated_text: s.transliterated_text,
            start_time: s.start_time,
            end_time: s.end_time,
            is_part_of_word: s.is_part_of_word,
        }
    }
}

impl From<FfiSyllable> for Syllable {
    fn from(s: FfiSyllable) -> Self {
        Self {
            text: s.text,
            transliterated_text: s.transliterated_text,
            start_time: s.start_time,
            end_time: s.end_time,
            is_part_of_word: s.is_part_of_word,
        }
    }
}

impl From<VocalTrack> for FfiVocalTrack {
    fn from(v: VocalTrack) -> Self {
        Self {
            start_time: v.start_time,
            end_time: v.end_time,
            syllables: v.syllables.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FfiVocalTrack> for VocalTrack {
    fn from(v: FfiVocalTrack) -> Self {
        Self {
            start_time: v.start_time,
            end_time: v.end_time,
            syllables: v.syllables.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<LyricsLine> for FfiLyricsLine {
    fn from(l: LyricsLine) -> Self {
        Self {
            start_time: l.start_time,
            end_time: l.end_time,
            opposite_aligned: l.opposite_aligned,
            lead: l.lead.into(),
            background: l.background.into_iter().map(Into::into).collect(),
            text: l.text,
            transliterated_text: l.transliterated_text,
        }
    }
}

impl From<FfiLyricsLine> for LyricsLine {
    fn from(l: FfiLyricsLine) -> Self {
        Self {
            start_time: l.start_time,
            end_time: l.end_time,
            opposite_aligned: l.opposite_aligned,
            lead: l.lead.into(),
            background: l.background.into_iter().map(Into::into).collect(),
            text: l.text,
            transliterated_text: l.transliterated_text,
        }
    }
}

impl From<LyricsDocument> for FfiLyricsDocument {
    fn from(d: LyricsDocument) -> Self {
        Self {
            kind: d.kind.into(),
            lines: d.lines.into_iter().map(Into::into).collect(),
            song_writers: d.song_writers,
        }
    }
}

impl From<FfiLyricsDocument> for LyricsDocument {
    fn from(d: FfiLyricsDocument) -> Self {
        Self {
            kind: d.kind.into(),
            lines: d.lines.into_iter().map(Into::into).collect(),
            song_writers: d.song_writers,
        }
    }
}

impl From<LyricsResult> for FfiLyricsResult {
    fn from(r: LyricsResult) -> Self {
        Self {
            document: r.document.into(),
            source: r.source.into(),
        }
    }
}

impl From<SyllableProgress> for FfiSyllableProgress {
    fn from(p: SyllableProgress) -> Self {
        Self {
            text: p.text,
            progress: p.progress,
            is_part_of_word: p.is_part_of_word,
        }
    }
}

impl From<SyncSnapshot> for FfiSyncSnapshot {
    fn from(s: SyncSnapshot) -> Self {
        Self {
            position_sec: s.position_sec,
            active_index: s.active_index.and_then(|i| u32::try_from(i).ok()),
            focus_index: s.focus_index.and_then(|i| u32::try_from(i).ok()),
            in_instrumental_gap: s.in_instrumental_gap,
            lead_syllables: s.lead_syllables.into_iter().map(Into::into).collect(),
            background_lines: s
                .background_lines
                .into_iter()
                .map(|line| line.into_iter().map(Into::into).collect())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_evaluates_sync_via_ffi_surface() {
        let client = LuckyMatchClient::new(FfiConfig {
            spotify_client_id: None,
            spotify_client_secret: None,
            spicy_lyrics_api_key: None,
            cache_dir: None,
            custom_lyrics_dir: None,
        })
        .expect("client init");

        let doc = client
            .parse_lyrics(
                "lrc".to_string(),
                "[00:05.00]First line\n[00:10.00]Second line".to_string(),
                Some(20_000),
            )
            .expect("lrc parse");
        assert_eq!(doc.kind, FfiLyricsKind::Line);
        assert_eq!(doc.lines.len(), 2);

        let snap = client.evaluate_sync(&doc, 7.5, 2.5);
        assert_eq!(snap.active_index, Some(0));
        assert!(!snap.in_instrumental_gap);
        assert!((snap.lead_syllables[0].progress - 0.5).abs() < 1e-6);
    }
}
