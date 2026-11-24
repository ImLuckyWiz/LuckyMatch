use std::collections::VecDeque;
use std::time::{Duration, Instant};

use luckymatch_core::audio::{rms_level, TARGET_SAMPLE_RATE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    Loopback,
    Mic,
}

impl CaptureSource {
    pub fn label(self) -> &'static str {
        match self {
            CaptureSource::Loopback => "System Audio",
            CaptureSource::Mic => "Microphone",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            CaptureSource::Loopback => CaptureSource::Mic,
            CaptureSource::Mic => CaptureSource::Loopback,
        }
    }
}

pub struct CapturedClip {
    pub samples_16khz_mono: Vec<i16>,
    pub capture_start: Instant,
    pub rms: f32,
}

/// Bounded rolling buffer of 16 kHz mono PCM samples that maintains the effective
/// wall-clock anchor of the oldest sample currently retained in the window.
pub struct RollingBuffer {
    samples: VecDeque<i16>,
    max_samples: usize,
    first_sample_at: Option<Instant>,
    dropped_samples: u64,
    total_received: u64,
}

impl RollingBuffer {
    pub fn new(max_window_secs: f32) -> Self {
        let clamped = max_window_secs.clamp(2.0, 12.0);
        let max_samples = (clamped * TARGET_SAMPLE_RATE as f32).round() as usize;
        Self {
            samples: VecDeque::with_capacity(max_samples + 2048),
            max_samples,
            first_sample_at: None,
            dropped_samples: 0,
            total_received: 0,
        }
    }

    pub fn push(&mut self, incoming: &[i16]) {
        if incoming.is_empty() {
            return;
        }
        if self.first_sample_at.is_none() {
            self.first_sample_at = Some(Instant::now());
        }
        for &s in incoming {
            self.samples.push_back(s);
        }
        self.total_received += incoming.len() as u64;
        while self.samples.len() > self.max_samples {
            self.samples.pop_front();
            self.dropped_samples += 1;
        }
    }

    pub fn total_received_secs(&self) -> f32 {
        self.total_received as f32 / TARGET_SAMPLE_RATE as f32
    }

    pub fn snapshot(&self) -> Option<CapturedClip> {
        let first_at = self.first_sample_at?;
        if self.samples.is_empty() {
            return None;
        }
        let samples_16khz_mono: Vec<i16> = self.samples.iter().copied().collect();
        let effective_start = first_at
            + Duration::from_secs_f64(self.dropped_samples as f64 / TARGET_SAMPLE_RATE as f64);
        let rms = rms_level(&samples_16khz_mono);
        Some(CapturedClip {
            samples_16khz_mono,
            capture_start: effective_start,
            rms,
        })
    }
}

#[cfg(windows)]
mod imp {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use luckymatch_core::audio::to_mono_16khz_i16_from_f32;
    use wasapi::{
        initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat,
    };

    use super::{CaptureSource, CapturedClip, RollingBuffer};

    const WASAPI_SAMPLE_RATE: usize = 48_000;
    const WASAPI_CHANNELS: usize = 2;

    fn reset_level(live_level: Option<&Arc<AtomicU32>>) {
        if let Some(atom) = live_level {
            atom.store(0.0f32.to_bits(), Ordering::Relaxed);
        }
    }

    fn update_level_from_bytes(
        bytes: &[u8],
        smoothed: &mut f32,
        live_level: Option<&Arc<AtomicU32>>,
    ) {
        let Some(atom) = live_level else {
            return;
        };
        if bytes.len() < 4 {
            return;
        }
        let mut sum_sq = 0.0f64;
        let mut count = 0usize;
        for c in bytes.chunks_exact(4) {
            let s = f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64;
            sum_sq += s * s;
            count += 1;
        }
        if count == 0 {
            return;
        }
        let rms = (sum_sq / count as f64).sqrt() as f32;
        let target = (rms * 6.5).clamp(0.0, 1.0);
        let coeff = if target > *smoothed { 0.65 } else { 0.22 };
        *smoothed += (target - *smoothed) * coeff;
        atom.store(smoothed.to_bits(), Ordering::Relaxed);
    }

    pub struct StreamingCapture {
        buffer: Arc<Mutex<RollingBuffer>>,
        error: Arc<Mutex<Option<String>>>,
        stop_flag: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
    }

    impl StreamingCapture {
        pub fn start(
            source: CaptureSource,
            max_window_secs: f32,
            live_level: Option<Arc<AtomicU32>>,
        ) -> Self {
            let buffer = Arc::new(Mutex::new(RollingBuffer::new(max_window_secs)));
            let error = Arc::new(Mutex::new(None));
            let stop_flag = Arc::new(AtomicBool::new(false));

            let buf_bg = buffer.clone();
            let err_bg = error.clone();
            let stop_bg = stop_flag.clone();

            let join = std::thread::Builder::new()
                .name("luckymatch-audio-stream".to_string())
                .spawn(move || {
                    if let Err(err) =
                        run_wasapi_stream(source, &buf_bg, &stop_bg, live_level.as_ref())
                    {
                        if let Ok(mut guard) = err_bg.lock() {
                            *guard = Some(err.to_string());
                        }
                    }
                    reset_level(live_level.as_ref());
                })
                .ok();

            Self {
                buffer,
                error,
                stop_flag,
                join,
            }
        }

        pub fn total_received_secs(&self) -> f32 {
            self.buffer
                .lock()
                .map(|b| b.total_received_secs())
                .unwrap_or(0.0)
        }

        pub fn snapshot(&self) -> Option<CapturedClip> {
            self.buffer.lock().ok().and_then(|b| b.snapshot())
        }

        pub fn take_error(&self) -> Option<String> {
            self.error.lock().ok().and_then(|mut e| e.take())
        }

        pub fn stop(&mut self) {
            self.stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = self.join.take() {
                let _ = handle.join();
            }
        }
    }

    impl Drop for StreamingCapture {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn run_wasapi_stream(
        source: CaptureSource,
        buffer: &Mutex<RollingBuffer>,
        stop_flag: &AtomicBool,
        live_level: Option<&Arc<AtomicU32>>,
    ) -> Result<()> {
        reset_level(live_level);
        let _ = initialize_mta();

        let enumerator = DeviceEnumerator::new().context("failed to create WASAPI enumerator")?;
        let device = match source {
            CaptureSource::Loopback => enumerator
                .get_default_device(Direction::Render)
                .context("no default playback device for loopback")?,
            CaptureSource::Mic => enumerator
                .get_default_device(Direction::Capture)
                .context("no default microphone capture device")?,
        };

        let mut audio_client = device
            .get_iaudioclient()
            .context("failed to get IAudioClient")?;

        let desired_format = WaveFormat::new(
            32,
            32,
            SampleType::Float,
            WASAPI_SAMPLE_RATE,
            WASAPI_CHANNELS,
            None,
        );
        let blockalign = desired_format.get_blockalign() as usize;
        // 48 kHz -> 16 kHz is an exact 3:1 ratio; aligning each conversion batch to
        // `blockalign * 3` (24 bytes) prevents fractional-frame drift across long streams.
        let quantum_bytes = blockalign * 3;

        let (def_time, _) = audio_client.get_device_period()?;
        let mode = StreamMode::PollingShared {
            autoconvert: true,
            buffer_duration_hns: def_time.max(200_000),
        };

        audio_client
            .initialize_client(&desired_format, Direction::Capture, mode, None)
            .context("failed to initialize WASAPI capture client")?;

        let capture_client = audio_client
            .get_audiocaptureclient()
            .context("failed to get IAudioCaptureClient")?;

        let mut sample_queue: VecDeque<u8> = VecDeque::with_capacity(blockalign * 4096);
        let mut pending_bytes: Vec<u8> = Vec::with_capacity(blockalign * 4096);
        let mut floats: Vec<f32> = Vec::with_capacity(2048);

        audio_client
            .start_stream()
            .context("failed to start WASAPI capture stream")?;

        let mut smoothed_level = 0.0f32;

        while !stop_flag.load(Ordering::Relaxed) {
            let next_frames = capture_client.get_next_packet_size()?.unwrap_or(0);
            if next_frames > 0 {
                capture_client.read_from_device_to_deque(&mut sample_queue)?;
                let prev_len = pending_bytes.len();
                pending_bytes.extend(sample_queue.drain(..));
                update_level_from_bytes(&pending_bytes[prev_len..], &mut smoothed_level, live_level);

                let usable = (pending_bytes.len() / quantum_bytes) * quantum_bytes;
                if usable > 0 {
                    floats.clear();
                    for chunk in pending_bytes[..usable].chunks_exact(4) {
                        floats.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                    }
                    let mono_16khz = to_mono_16khz_i16_from_f32(
                        &floats,
                        WASAPI_SAMPLE_RATE as u32,
                        WASAPI_CHANNELS as u16,
                    );
                    pending_bytes.drain(..usable);

                    if let Ok(mut guard) = buffer.lock() {
                        guard.push(&mono_16khz);
                    }
                }
            } else {
                smoothed_level *= 0.88;
                if let Some(atom) = live_level {
                    atom.store(smoothed_level.to_bits(), Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        let _ = audio_client.stop_stream();
        reset_level(live_level);
        Ok(())
    }

    pub fn capture_clip(
        source: CaptureSource,
        seconds: f32,
        cancel: &AtomicBool,
        live_level: Option<Arc<AtomicU32>>,
    ) -> Result<Option<CapturedClip>> {
        let clamped = seconds.clamp(2.0, 12.0);
        let mut stream = StreamingCapture::start(source, clamped, live_level);
        let deadline = Instant::now() + Duration::from_secs_f32(clamped + 5.0);

        loop {
            if cancel.load(Ordering::Relaxed) {
                stream.stop();
                return Ok(None);
            }
            if let Some(err) = stream.take_error() {
                stream.stop();
                anyhow::bail!("{err}");
            }
            if stream.total_received_secs() >= clamped {
                break;
            }
            if Instant::now() > deadline {
                if stream.total_received_secs() <= 0.0 {
                    stream.stop();
                    return Ok(None);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let clip = stream.snapshot();
        stream.stop();
        Ok(clip)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::sync::Arc;

    use anyhow::{bail, Result};

    use super::{CaptureSource, CapturedClip};

    pub struct StreamingCapture;

    impl StreamingCapture {
        pub fn start(
            _source: CaptureSource,
            _max_window_secs: f32,
            _live_level: Option<Arc<AtomicU32>>,
        ) -> Self {
            Self
        }

        pub fn total_received_secs(&self) -> f32 {
            0.0
        }

        pub fn snapshot(&self) -> Option<CapturedClip> {
            None
        }

        pub fn take_error(&self) -> Option<String> {
            Some("WASAPI capture is only supported on Windows".to_string())
        }

        pub fn stop(&mut self) {}
    }

    pub fn capture_clip(
        _source: CaptureSource,
        _seconds: f32,
        _cancel: &AtomicBool,
        _live_level: Option<Arc<AtomicU32>>,
    ) -> Result<Option<CapturedClip>> {
        bail!("WASAPI capture is only supported on Windows")
    }
}

pub use imp::{capture_clip, StreamingCapture};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_buffer_slides_anchor_as_samples_drop() {
        let mut buf = RollingBuffer::new(2.0); // 32,000 max samples
        let chunk_1s = vec![1000i16; 16_000];

        buf.push(&chunk_1s);
        let first_snap = buf.snapshot().expect("snapshot after 1s");
        assert_eq!(first_snap.samples_16khz_mono.len(), 16_000);
        let initial_anchor = first_snap.capture_start;

        buf.push(&chunk_1s);
        buf.push(&chunk_1s); // 48,000 total -> 16,000 dropped (1.0s)

        assert!((buf.total_received_secs() - 3.0).abs() < 0.001);
        let slid_snap = buf.snapshot().expect("snapshot after 3s");
        assert_eq!(slid_snap.samples_16khz_mono.len(), 32_000);
        let anchor_advance = slid_snap
            .capture_start
            .saturating_duration_since(initial_anchor)
            .as_secs_f64();
        assert!(
            (anchor_advance - 1.0).abs() < 0.001,
            "expected anchor to advance 1.0s, got {anchor_advance}"
        );
    }
}
