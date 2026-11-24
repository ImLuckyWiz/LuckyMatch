use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use gpui::RenderImage;
use image::Frame;
use luckymatch_core::{
    LuckyMatchConfig, LuckyMatchEngine, LyricsQuery, LyricsResult, RecognizedTrack,
};
use smallvec::SmallVec;
use tokio::sync::mpsc as tokio_mpsc;

use crate::audio_capture::{capture_clip, CaptureSource, StreamingCapture};

const AUTO_SYNC_CAPTURE_SECONDS: f32 = 10.5;
const PROGRESSIVE_WINDOW_SECONDS: f32 = 12.0;
const PROGRESSIVE_CHECKPOINTS: [f32; 3] = [3.5, 7.0, 11.5];
const PROGRESSIVE_SLIDE_STEP_SECONDS: f32 = 4.5;
const PROGRESSIVE_MAX_SECONDS: f32 = 45.0;
const AUTO_REVERIFY_SECS: u64 = 45;
const SILENCE_RMS_THRESHOLD: f32 = 0.0005;

#[derive(Debug, Clone)]
pub enum BackendStatus {
    Idle,
    Capturing {
        source: CaptureSource,
        started_at: Instant,
        duration_sec: Option<f32>,
    },
    Identifying,
    SilentAudio,
    NoMatch,
    Error(String),
}

#[derive(Debug)]
pub enum BackendCommand {
    IdentifyNow,
    CancelIdentify,
    SetSource(CaptureSource),
    SetAutoSync(bool),
    Shutdown,
}

pub enum BackendEvent {
    StatusChanged(BackendStatus),
    TrackMatched {
        track: RecognizedTrack,
        same_song: bool,
        is_manual: bool,
    },
    LyricsUpdated(LyricsResult),
    CoverLoaded {
        art_url: String,
        image: Arc<RenderImage>,
        accent_rgb: (u8, u8, u8),
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackSignature {
    isrc: Option<String>,
    spotify_id: Option<String>,
    title_artist_norm: String,
}

impl TrackSignature {
    fn from_track(track: &RecognizedTrack) -> Self {
        let norm = format!(
            "{}|{}",
            track.recognition.title.trim().to_lowercase(),
            track.recognition.artist.trim().to_lowercase()
        );
        Self {
            isrc: track.recognition.isrc.clone(),
            spotify_id: track.recognition.spotify_track_id.clone(),
            title_artist_norm: norm,
        }
    }

    fn matches(&self, other: &Self) -> bool {
        if self.spotify_id.is_some() && self.spotify_id == other.spotify_id {
            return true;
        }
        if self.isrc.is_some() && self.isrc == other.isrc {
            return true;
        }
        self.title_artist_norm == other.title_artist_norm
    }
}

pub struct BackendHandle {
    pub cmd_tx: tokio_mpsc::UnboundedSender<BackendCommand>,
    pub event_rx: std_mpsc::Receiver<BackendEvent>,
    pub live_level: Arc<AtomicU32>,
}

pub fn spawn_backend() -> BackendHandle {
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = std_mpsc::channel();
    let live_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let live_level_worker = live_level.clone();

    std::thread::Builder::new()
        .name("luckymatch-worker".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    let _ = event_tx.send(BackendEvent::StatusChanged(BackendStatus::Error(
                        format!("Failed to start Tokio runtime: {err}"),
                    )));
                    return;
                }
            };

            rt.block_on(worker_loop(cmd_rx, event_tx, live_level_worker));
        })
        .expect("failed to spawn luckymatch-worker thread");

    BackendHandle {
        cmd_tx,
        event_rx,
        live_level,
    }
}

fn load_desktop_config() -> LuckyMatchConfig {
    let cwd_cfg = PathBuf::from("config.json");
    let config_path = if cwd_cfg.exists() {
        Some(cwd_cfg)
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|dir| dir.join("config.json")))
            .filter(|p| p.exists())
    };

    let mut cfg = LuckyMatchConfig::load(config_path.as_deref());
    if cfg.cache_dir.is_none() {
        cfg.cache_dir = Some(PathBuf::from("cache").join("lyrics"));
    }
    if cfg.custom_lyrics_dir.is_none() {
        cfg.custom_lyrics_dir = Some(PathBuf::from("lyrics_custom"));
    }
    cfg
}

struct WorkerState<'a> {
    engine: &'a LuckyMatchEngine,
    cmd_rx: &'a mut tokio_mpsc::UnboundedReceiver<BackendCommand>,
    event_tx: &'a std_mpsc::Sender<BackendEvent>,
    live_level: &'a Arc<AtomicU32>,
    source: CaptureSource,
    auto_sync: bool,
    current_sig: Option<TrackSignature>,
    lyrics_task: Option<tokio::task::JoinHandle<()>>,
    cover_task: Option<tokio::task::JoinHandle<()>>,
    next_auto_at: Option<Instant>,
}

impl<'a> WorkerState<'a> {
    fn schedule_retry_if_active(&mut self, secs: u64) {
        self.next_auto_at = if self.auto_sync && self.current_sig.is_some() {
            Some(Instant::now() + Duration::from_secs(secs))
        } else {
            None
        };
    }

    fn set_auto_sync(&mut self, enabled: bool) {
        self.auto_sync = enabled;
        if self.auto_sync && self.current_sig.is_some() {
            self.next_auto_at = Some(Instant::now() + Duration::from_secs(AUTO_REVERIFY_SECS));
        } else if !self.auto_sync {
            self.next_auto_at = None;
        }
    }

    fn handle_matched_track(&mut self, track: RecognizedTrack, is_manual: bool) {
        let sig = TrackSignature::from_track(&track);
        let same_song = self
            .current_sig
            .as_ref()
            .is_some_and(|prev| prev.matches(&sig));
        self.current_sig = Some(sig);

        let album_art = track.recognition.album_art.clone();
        let duration_ms = track.spotify.as_ref().map(|s| s.duration_ms);
        let pos_now = track.clock.position_now();
        let query = LyricsQuery {
            title: track.recognition.title.clone(),
            artist: track.recognition.artist.clone(),
            spotify_track_id: track.recognition.spotify_track_id.clone(),
            isrc: track.recognition.isrc.clone(),
            duration_ms,
        };

        let _ = self.event_tx.send(BackendEvent::TrackMatched {
            track,
            same_song,
            is_manual,
        });
        let _ = self
            .event_tx
            .send(BackendEvent::StatusChanged(BackendStatus::Idle));

        if !same_song {
            if let Some(h) = self.lyrics_task.take() {
                h.abort();
            }
            let mut rx = self.engine.fetch_lyrics_streaming(query);
            let tx_l = self.event_tx.clone();
            self.lyrics_task = Some(tokio::spawn(async move {
                while let Some(res) = rx.recv().await {
                    let _ = tx_l.send(BackendEvent::LyricsUpdated(res));
                }
            }));

            if let Some(h) = self.cover_task.take() {
                h.abort();
            }
            if let Some(url) = album_art {
                let engine_c = self.engine.clone();
                let tx_c = self.event_tx.clone();
                self.cover_task = Some(tokio::spawn(async move {
                    if let Some((image, accent_rgb)) = fetch_cover_image(&engine_c, &url).await {
                        let _ = tx_c.send(BackendEvent::CoverLoaded {
                            art_url: url,
                            image,
                            accent_rgb,
                        });
                    }
                }));
            }
        }

        self.next_auto_at = if self.auto_sync {
            let wait_secs = match duration_ms.filter(|&d| d > 0) {
                Some(d) => {
                    let rem = (d as f64 / 1000.0 - pos_now + 3.0).max(12.0);
                    (rem.round() as u64).min(AUTO_REVERIFY_SECS)
                }
                None => AUTO_REVERIFY_SECS,
            };
            Some(Instant::now() + Duration::from_secs(wait_secs))
        } else {
            None
        };
    }
}

enum ProgressiveOutcome {
    Shutdown,
    Cancelled,
    Matched(RecognizedTrack),
    TimedOut { had_non_silent: bool },
    Error(String),
}

async fn run_progressive_identify(state: &mut WorkerState<'_>) -> ProgressiveOutcome {
    'source_restart: loop {
        let capture_source = state.source;
        let started_at = Instant::now();
        let _ = state
            .event_tx
            .send(BackendEvent::StatusChanged(BackendStatus::Capturing {
                source: capture_source,
                started_at,
                duration_sec: None,
            }));

        let mut stream = StreamingCapture::start(
            capture_source,
            PROGRESSIVE_WINDOW_SECONDS,
            Some(state.live_level.clone()),
        );
        let mut checkpoint_idx = 0usize;
        let mut next_check_sec = PROGRESSIVE_CHECKPOINTS[0];
        let mut had_non_silent = false;

        loop {
            if let Some(err) = stream.take_error() {
                stream.stop();
                return ProgressiveOutcome::Error(err);
            }

            if next_check_sec > PROGRESSIVE_MAX_SECONDS
                || started_at.elapsed() > Duration::from_secs_f32(PROGRESSIVE_MAX_SECONDS + 3.0)
            {
                stream.stop();
                return ProgressiveOutcome::TimedOut { had_non_silent };
            }

            if stream.total_received_secs() >= next_check_sec {
                checkpoint_idx += 1;
                if checkpoint_idx < PROGRESSIVE_CHECKPOINTS.len() {
                    next_check_sec = PROGRESSIVE_CHECKPOINTS[checkpoint_idx];
                } else {
                    next_check_sec += PROGRESSIVE_SLIDE_STEP_SECONDS;
                }

                if let Some(clip) = stream.snapshot() {
                    if clip.rms >= SILENCE_RMS_THRESHOLD {
                        had_non_silent = true;
                        let _ = state
                            .event_tx
                            .send(BackendEvent::StatusChanged(BackendStatus::Identifying));

                        let recog_fut = state.engine.recognize_pcm_i16(
                            &clip.samples_16khz_mono,
                            luckymatch_core::audio::TARGET_SAMPLE_RATE,
                            1,
                            clip.capture_start,
                        );
                        tokio::pin!(recog_fut);

                        let recog_res = loop {
                            tokio::select! {
                                res = &mut recog_fut => break Some(res),
                                cmd = state.cmd_rx.recv() => {
                                    match cmd {
                                        Some(BackendCommand::CancelIdentify) => {
                                            stream.stop();
                                            return ProgressiveOutcome::Cancelled;
                                        }
                                        Some(BackendCommand::SetSource(s)) => {
                                            state.source = s;
                                            stream.stop();
                                            continue 'source_restart;
                                        }
                                        Some(BackendCommand::SetAutoSync(e)) => state.set_auto_sync(e),
                                        Some(BackendCommand::IdentifyNow) => {}
                                        Some(BackendCommand::Shutdown) | None => {
                                            stream.stop();
                                            return ProgressiveOutcome::Shutdown;
                                        }
                                    }
                                }
                            }
                        };

                        if let Some(res) = recog_res {
                            match res {
                                Ok(Some(track)) => {
                                    stream.stop();
                                    return ProgressiveOutcome::Matched(track);
                                }
                                Ok(None) => {
                                    let _ = state.event_tx.send(BackendEvent::StatusChanged(
                                        BackendStatus::Capturing {
                                            source: capture_source,
                                            started_at,
                                            duration_sec: None,
                                        },
                                    ));
                                }
                                Err(err) => {
                                    stream.stop();
                                    return ProgressiveOutcome::Error(err.to_string());
                                }
                            }
                        }
                    }
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                cmd = state.cmd_rx.recv() => {
                    match cmd {
                        Some(BackendCommand::CancelIdentify) => {
                            stream.stop();
                            return ProgressiveOutcome::Cancelled;
                        }
                        Some(BackendCommand::SetSource(s)) => {
                            state.source = s;
                            stream.stop();
                            continue 'source_restart;
                        }
                        Some(BackendCommand::SetAutoSync(e)) => state.set_auto_sync(e),
                        Some(BackendCommand::IdentifyNow) => {}
                        Some(BackendCommand::Shutdown) | None => {
                            stream.stop();
                            return ProgressiveOutcome::Shutdown;
                        }
                    }
                }
            }
        }
    }
}

enum AutoSyncOutcome {
    Shutdown,
    Cancelled,
    PromoteToManual,
    Continue,
}

async fn run_auto_sync_identify(state: &mut WorkerState<'_>) -> AutoSyncOutcome {
    let source = state.source;
    let _ = state
        .event_tx
        .send(BackendEvent::StatusChanged(BackendStatus::Capturing {
            source,
            started_at: Instant::now(),
            duration_sec: Some(AUTO_SYNC_CAPTURE_SECONDS),
        }));

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();
    let level_clone = state.live_level.clone();
    let mut capture_handle = tokio::task::spawn_blocking(move || {
        capture_clip(source, AUTO_SYNC_CAPTURE_SECONDS, &cancel_clone, Some(level_clone))
    });

    let mut cancelled = false;
    let mut promote_manual = false;
    let join_res = loop {
        tokio::select! {
            res = &mut capture_handle => break res,
            cmd = state.cmd_rx.recv() => {
                match cmd {
                    Some(BackendCommand::CancelIdentify) => {
                        cancelled = true;
                        cancel.store(true, Ordering::Relaxed);
                    }
                    Some(BackendCommand::IdentifyNow) => {
                        promote_manual = true;
                        cancel.store(true, Ordering::Relaxed);
                    }
                    Some(BackendCommand::SetSource(s)) => {
                        state.source = s;
                        cancelled = true;
                        cancel.store(true, Ordering::Relaxed);
                    }
                    Some(BackendCommand::SetAutoSync(enabled)) => state.set_auto_sync(enabled),
                    Some(BackendCommand::Shutdown) | None => {
                        cancel.store(true, Ordering::Relaxed);
                        let _ = capture_handle.await;
                        return AutoSyncOutcome::Shutdown;
                    }
                }
            }
        }
    };

    if promote_manual {
        return AutoSyncOutcome::PromoteToManual;
    }
    if cancelled {
        let _ = state
            .event_tx
            .send(BackendEvent::StatusChanged(BackendStatus::Idle));
        return AutoSyncOutcome::Cancelled;
    }

    let clip = match join_res {
        Ok(Ok(Some(c))) => c,
        Ok(Ok(None)) => {
            let _ = state
                .event_tx
                .send(BackendEvent::StatusChanged(BackendStatus::SilentAudio));
            state.schedule_retry_if_active(12);
            return AutoSyncOutcome::Continue;
        }
        Ok(Err(err)) => {
            let _ = state
                .event_tx
                .send(BackendEvent::StatusChanged(BackendStatus::Error(err.to_string())));
            state.schedule_retry_if_active(15);
            return AutoSyncOutcome::Continue;
        }
        Err(err) => {
            let _ = state
                .event_tx
                .send(BackendEvent::StatusChanged(BackendStatus::Error(err.to_string())));
            state.schedule_retry_if_active(15);
            return AutoSyncOutcome::Continue;
        }
    };

    if clip.rms < SILENCE_RMS_THRESHOLD {
        let _ = state
            .event_tx
            .send(BackendEvent::StatusChanged(BackendStatus::SilentAudio));
        state.schedule_retry_if_active(12);
        return AutoSyncOutcome::Continue;
    }

    let _ = state
        .event_tx
        .send(BackendEvent::StatusChanged(BackendStatus::Identifying));

    let recog_fut = state.engine.recognize_pcm_i16(
        &clip.samples_16khz_mono,
        luckymatch_core::audio::TARGET_SAMPLE_RATE,
        1,
        clip.capture_start,
    );
    tokio::pin!(recog_fut);

    let recog_res = loop {
        tokio::select! {
            res = &mut recog_fut => break res,
            cmd = state.cmd_rx.recv() => {
                match cmd {
                    Some(BackendCommand::CancelIdentify) => {
                        let _ = state
                            .event_tx
                            .send(BackendEvent::StatusChanged(BackendStatus::Idle));
                        return AutoSyncOutcome::Cancelled;
                    }
                    Some(BackendCommand::IdentifyNow) => return AutoSyncOutcome::PromoteToManual,
                    Some(BackendCommand::SetSource(s)) => state.source = s,
                    Some(BackendCommand::SetAutoSync(e)) => state.set_auto_sync(e),
                    Some(BackendCommand::Shutdown) | None => return AutoSyncOutcome::Shutdown,
                }
            }
        }
    };

    match recog_res {
        Ok(Some(track)) => {
            state.handle_matched_track(track, false);
        }
        Ok(None) => {
            let _ = state
                .event_tx
                .send(BackendEvent::StatusChanged(BackendStatus::NoMatch));
            state.schedule_retry_if_active(12);
        }
        Err(err) => {
            let _ = state
                .event_tx
                .send(BackendEvent::StatusChanged(BackendStatus::Error(err.to_string())));
            state.schedule_retry_if_active(15);
        }
    }

    AutoSyncOutcome::Continue
}

async fn worker_loop(
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<BackendCommand>,
    event_tx: std_mpsc::Sender<BackendEvent>,
    live_level: Arc<AtomicU32>,
) {
    let cfg = load_desktop_config();
    let engine = match LuckyMatchEngine::new(cfg) {
        Ok(e) => e,
        Err(err) => {
            let _ = event_tx.send(BackendEvent::StatusChanged(BackendStatus::Error(
                err.to_string(),
            )));
            return;
        }
    };

    let mut state = WorkerState {
        engine: &engine,
        cmd_rx: &mut cmd_rx,
        event_tx: &event_tx,
        live_level: &live_level,
        source: CaptureSource::Loopback,
        auto_sync: true,
        current_sig: None,
        lyrics_task: None,
        cover_task: None,
        // Wait for the user's first tap/R on launch; auto-sync starts once a track has been matched.
        next_auto_at: None,
    };

    loop {
        let sleep_dur = match state.next_auto_at {
            Some(at) if state.auto_sync => at
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(250)),
            _ => Duration::from_millis(250),
        };

        let mut trigger_identify = false;
        let mut is_manual = false;
        tokio::select! {
            cmd = state.cmd_rx.recv() => {
                match cmd {
                    Some(BackendCommand::IdentifyNow) => {
                        trigger_identify = true;
                        is_manual = true;
                    }
                    Some(BackendCommand::CancelIdentify) => {
                        let _ = state.event_tx.send(BackendEvent::StatusChanged(BackendStatus::Idle));
                    }
                    Some(BackendCommand::SetSource(s)) => state.source = s,
                    Some(BackendCommand::SetAutoSync(enabled)) => state.set_auto_sync(enabled),
                    Some(BackendCommand::Shutdown) | None => break,
                }
            }
            _ = tokio::time::sleep(sleep_dur) => {
                if state.auto_sync && state.next_auto_at.is_some_and(|at| Instant::now() >= at) {
                    trigger_identify = true;
                }
            }
        }

        if !trigger_identify {
            continue;
        }

        // Drain queued commands before starting capture.
        while let Ok(cmd) = state.cmd_rx.try_recv() {
            match cmd {
                BackendCommand::IdentifyNow => {
                    trigger_identify = true;
                    is_manual = true;
                }
                BackendCommand::CancelIdentify => {
                    trigger_identify = false;
                    is_manual = false;
                    let _ = state
                        .event_tx
                        .send(BackendEvent::StatusChanged(BackendStatus::Idle));
                }
                BackendCommand::SetSource(s) => state.source = s,
                BackendCommand::SetAutoSync(enabled) => state.set_auto_sync(enabled),
                BackendCommand::Shutdown => return,
            }
        }

        if !trigger_identify {
            continue;
        }

        if !is_manual {
            match run_auto_sync_identify(&mut state).await {
                AutoSyncOutcome::Shutdown => return,
                AutoSyncOutcome::Cancelled | AutoSyncOutcome::Continue => continue,
                AutoSyncOutcome::PromoteToManual => {
                    // Fall through immediately into progressive manual identify.
                }
            }
        }

        match run_progressive_identify(&mut state).await {
            ProgressiveOutcome::Shutdown => return,
            ProgressiveOutcome::Cancelled => {
                let _ = state
                    .event_tx
                    .send(BackendEvent::StatusChanged(BackendStatus::Idle));
                state.schedule_retry_if_active(AUTO_REVERIFY_SECS);
            }
            ProgressiveOutcome::Matched(track) => {
                state.handle_matched_track(track, true);
            }
            ProgressiveOutcome::TimedOut { had_non_silent } => {
                let status = if had_non_silent {
                    BackendStatus::NoMatch
                } else {
                    BackendStatus::SilentAudio
                };
                let _ = state.event_tx.send(BackendEvent::StatusChanged(status));
                state.schedule_retry_if_active(12);
            }
            ProgressiveOutcome::Error(err) => {
                let _ = state
                    .event_tx
                    .send(BackendEvent::StatusChanged(BackendStatus::Error(err)));
                state.schedule_retry_if_active(15);
            }
        }
    }
}

async fn fetch_cover_image(
    engine: &LuckyMatchEngine,
    url: &str,
) -> Option<(Arc<RenderImage>, (u8, u8, u8))> {
    let bytes = engine.fetch_bytes(url).await.ok()?;
    tokio::task::spawn_blocking(move || decode_bgra_and_accent(&bytes))
        .await
        .ok()?
}

pub(crate) fn decode_bgra_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory(bytes).ok()?.into_rgba8();
    // GPUI DirectX renderer expects BGRA pixels.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let frame = Frame::new(rgba);
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(frame, 1))))
}

fn decode_bgra_and_accent(bytes: &[u8]) -> Option<(Arc<RenderImage>, (u8, u8, u8))> {
    let mut rgba = image::load_from_memory(bytes).ok()?.into_rgba8();
    let accent = extract_accent_rgb(&rgba);

    // GPUI DirectX renderer expects BGRA pixels.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    let frame = Frame::new(rgba);
    let render_img = Arc::new(RenderImage::new(SmallVec::from_elem(frame, 1)));
    Some((render_img, accent))
}

fn extract_accent_rgb(rgba: &image::RgbaImage) -> (u8, u8, u8) {
    let mut r_sum = 0u64;
    let mut g_sum = 0u64;
    let mut b_sum = 0u64;
    let mut count = 0u64;

    let raw = rgba.as_raw();
    for pixel in raw.chunks_exact(16) {
        let (r, g, b, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
        if a < 128 {
            continue;
        }
        let max_c = r.max(g).max(b);
        let min_c = r.min(g).min(b);
        let sat = max_c - min_c;
        if max_c < 45 || min_c > 220 || sat < 28 {
            continue;
        }
        r_sum += r as u64;
        g_sum += g as u64;
        b_sum += b as u64;
        count += 1;
    }

    if count == 0 {
        return (56, 189, 248);
    }

    let r = ((r_sum / count) as u8).clamp(40, 235);
    let g = ((g_sum / count) as u8).clamp(40, 235);
    let b = ((b_sum / count) as u8).clamp(40, 235);
    (r, g, b)
}
