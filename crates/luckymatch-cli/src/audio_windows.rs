#[cfg(windows)]
mod imp {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use luckymatch_core::audio::{rms_level, to_mono_16khz_i16_from_f32};
    use wasapi::{
        initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CaptureSource {
        Loopback,
        Mic,
    }

    pub struct CapturedAudio {
        pub samples_16khz_mono: Vec<i16>,
        pub capture_start: Instant,
        pub rms: f32,
    }

    pub fn capture_audio(source: CaptureSource, seconds: f32) -> Result<CapturedAudio> {
        let seconds = seconds.clamp(2.0, 15.0);
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

        let sample_rate = 48_000usize;
        let channels = 2usize;
        let desired_format =
            WaveFormat::new(32, 32, SampleType::Float, sample_rate, channels, None);
        let blockalign = desired_format.get_blockalign() as usize;

        let (def_time, _) = audio_client.get_device_period()?;
        let mode = StreamMode::PollingShared {
            autoconvert: true,
            buffer_duration_hns: def_time.max(200_000), // >= 20ms
        };

        audio_client
            .initialize_client(&desired_format, Direction::Capture, mode, None)
            .context("failed to initialize WASAPI capture client")?;

        let capture_client = audio_client
            .get_audiocaptureclient()
            .context("failed to get IAudioCaptureClient")?;

        let target_frames = (sample_rate as f32 * seconds).round() as usize;
        let target_bytes = target_frames * blockalign;
        let mut raw_bytes: Vec<u8> = Vec::with_capacity(target_bytes + blockalign * 1024);
        let mut sample_queue: VecDeque<u8> = VecDeque::with_capacity(blockalign * 4096);

        audio_client
            .start_stream()
            .context("failed to start WASAPI capture stream")?;

        let mut capture_start: Option<Instant> = None;
        let deadline = Instant::now() + Duration::from_secs_f32(seconds + 8.0);

        while raw_bytes.len() < target_bytes {
            if Instant::now() > deadline {
                let _ = audio_client.stop_stream();
                if raw_bytes.is_empty() {
                    bail!(
                        "no audio frames received within timeout (if using loopback, make sure audio is playing)"
                    );
                }
                break;
            }

            let next_frames = capture_client.get_next_packet_size()?.unwrap_or(0);
            if next_frames > 0 {
                if capture_start.is_none() {
                    capture_start = Some(Instant::now());
                }
                capture_client.read_from_device_to_deque(&mut sample_queue)?;
                while let Some(b) = sample_queue.pop_front() {
                    raw_bytes.push(b);
                    if raw_bytes.len() >= target_bytes {
                        break;
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        let _ = audio_client.stop_stream();

        let anchor = capture_start.unwrap_or_else(Instant::now);
        let sample_count = raw_bytes.len() / 4;
        let mut floats = Vec::with_capacity(sample_count);
        for chunk in raw_bytes.chunks_exact(4) {
            floats.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }

        let mono_16khz = to_mono_16khz_i16_from_f32(&floats, sample_rate as u32, channels as u16);
        let rms = rms_level(&mono_16khz);

        Ok(CapturedAudio {
            samples_16khz_mono: mono_16khz,
            capture_start: anchor,
            rms,
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use std::time::Instant;

    use anyhow::{bail, Result};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CaptureSource {
        Loopback,
        Mic,
    }

    pub struct CapturedAudio {
        pub samples_16khz_mono: Vec<i16>,
        pub capture_start: Instant,
        pub rms: f32,
    }

    pub fn capture_audio(_source: CaptureSource, _seconds: f32) -> Result<CapturedAudio> {
        bail!("WASAPI live capture is only available on Windows; use the `file` or `lyrics` subcommand")
    }
}

pub use imp::{capture_audio, CaptureSource};
