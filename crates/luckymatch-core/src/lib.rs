pub mod audio;
pub mod config;
pub mod error;
pub mod lyrics;
pub mod metadata;
pub mod recognition;
pub mod sync;

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

pub use config::LuckyMatchConfig;
pub use error::CoreError;
pub use lyrics::{
    fetch_lyrics, fetch_lyrics_streaming, LyricsDocument, LyricsKind, LyricsLine, LyricsQuery,
    LyricsResult, LyricsSource, Syllable, VocalTrack,
};
pub use metadata::{SpotifyResolver, SpotifyTrackInfo};
pub use recognition::{recognize_pcm_16khz, RecognitionMatch};
pub use sync::{evaluate_lyrics, PlaybackClock, SyllableProgress, SyncSnapshot};

/// Combined result of audio recognition, optional Spotify metadata resolution, and anchored clock.
#[derive(Debug, Clone)]
pub struct RecognizedTrack {
    pub recognition: RecognitionMatch,
    pub spotify: Option<SpotifyTrackInfo>,
    pub clock: PlaybackClock,
}

/// High-level portable facade for song recognition, metadata resolution, and lyrics fetching.
#[derive(Clone)]
pub struct LuckyMatchEngine {
    config: LuckyMatchConfig,
    client: reqwest::Client,
    spotify: SpotifyResolver,
}

impl LuckyMatchEngine {
    pub fn new(config: LuckyMatchConfig) -> Result<Self, CoreError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| CoreError::Http(format!("failed to build HTTP client: {e}")))?;

        let spotify = SpotifyResolver::new(
            client.clone(),
            config.spotify_client_id.clone(),
            config.spotify_client_secret.clone(),
        );

        Ok(Self {
            config,
            client,
            spotify,
        })
    }

    pub fn config(&self) -> &LuckyMatchConfig {
        &self.config
    }

    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Fetch raw bytes from an HTTP(S) URL (e.g., album artwork).
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, CoreError> {
        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| CoreError::Http(format!("GET {url} failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(CoreError::Http(format!(
                "GET {url} returned HTTP {}",
                resp.status()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| CoreError::Http(format!("reading body from {url} failed: {e}")))?;
        Ok(bytes.to_vec())
    }

    /// Recognize a song from arbitrary interleaved `i16` PCM audio anchored at `capture_start`.
    pub async fn recognize_pcm_i16(
        &self,
        samples: &[i16],
        sample_rate: u32,
        channels: u16,
        capture_start: Instant,
    ) -> Result<Option<RecognizedTrack>, CoreError> {
        let mono_16khz = audio::to_mono_16khz_i16_from_i16(samples, sample_rate, channels);
        self.recognize_mono_16khz(mono_16khz, capture_start).await
    }

    /// Recognize a song from arbitrary interleaved `f32` PCM audio anchored at `capture_start`.
    pub async fn recognize_pcm_f32(
        &self,
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
        capture_start: Instant,
    ) -> Result<Option<RecognizedTrack>, CoreError> {
        let mono_16khz = audio::to_mono_16khz_i16_from_f32(samples, sample_rate, channels);
        self.recognize_mono_16khz(mono_16khz, capture_start).await
    }

    async fn recognize_mono_16khz(
        &self,
        mono_16khz: Vec<i16>,
        capture_start: Instant,
    ) -> Result<Option<RecognizedTrack>, CoreError> {
        let Some(mut recognition) = recognize_pcm_16khz(&self.client, mono_16khz).await? else {
            return Ok(None);
        };

        let spotify = match self
            .spotify
            .resolve(
                recognition.isrc.as_deref(),
                &recognition.title,
                &recognition.artist,
            )
            .await
        {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!("Spotify track lookup skipped/failed: {err}");
                None
            }
        };

        if recognition.spotify_track_id.is_none() {
            if let Some(sp) = &spotify {
                recognition.spotify_track_id = Some(sp.track_id.clone());
            }
        }
        if recognition.album_art.is_none() {
            if let Some(sp) = &spotify {
                recognition.album_art = sp.album_art.clone();
            }
        }

        let clock = PlaybackClock::new(
            recognition.offset_sec,
            recognition.timeskew,
            capture_start,
        );

        Ok(Some(RecognizedTrack {
            recognition,
            spotify,
            clock,
        }))
    }

    /// Stream lyrics from the 6-tier priority waterfall (fast fallback first, syllable upgrade after).
    pub fn fetch_lyrics_streaming(
        &self,
        query: LyricsQuery,
    ) -> mpsc::UnboundedReceiver<LyricsResult> {
        fetch_lyrics_streaming(self.client.clone(), self.config.clone(), query)
    }

    /// Fetch the best available lyrics document from the 6-tier waterfall.
    pub async fn fetch_lyrics(&self, query: &LyricsQuery) -> Option<LyricsResult> {
        fetch_lyrics(&self.client, &self.config, query).await
    }
}
