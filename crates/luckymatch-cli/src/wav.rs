use std::path::Path;

use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavReader};
use luckymatch_core::audio::{to_mono_16khz_i16_from_f32, to_mono_16khz_i16_from_i16};

pub fn load_wav_16khz_mono(path: &Path, start_sec: f32, max_seconds: f32) -> Result<Vec<i16>> {
    let mut reader =
        WavReader::open(path).with_context(|| format!("failed to open WAV {}", path.display()))?;
    let spec = reader.spec();
    let sr = spec.sample_rate;
    let ch = spec.channels;
    if sr == 0 || ch == 0 {
        bail!("invalid WAV spec: sample_rate={sr}, channels={ch}");
    }

    let skip_samples = (start_sec.max(0.0) * sr as f32) as usize * ch as usize;
    let take_samples = (max_seconds.clamp(2.0, 15.0) * sr as f32) as usize * ch as usize;

    let mono_16khz = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => {
            let samples: Vec<f32> = reader
                .samples::<f32>()
                .skip(skip_samples)
                .take(take_samples)
                .collect::<std::result::Result<_, _>>()
                .context("failed to read float WAV samples")?;
            to_mono_16khz_i16_from_f32(&samples, sr, ch)
        }
        (SampleFormat::Int, bits) if bits <= 16 => {
            let shift = 16 - bits as i32;
            let samples: Vec<i16> = reader
                .samples::<i16>()
                .skip(skip_samples)
                .take(take_samples)
                .map(|r| r.map(|v| v << shift))
                .collect::<std::result::Result<_, _>>()
                .context("failed to read int16 WAV samples")?;
            to_mono_16khz_i16_from_i16(&samples, sr, ch)
        }
        (SampleFormat::Int, bits) if bits <= 32 => {
            let scale = ((1i64 << (bits - 1)) - 1) as f32;
            let samples: Vec<f32> = reader
                .samples::<i32>()
                .skip(skip_samples)
                .take(take_samples)
                .map(|r| r.map(|v| v as f32 / scale))
                .collect::<std::result::Result<_, _>>()
                .context("failed to read int32 WAV samples")?;
            to_mono_16khz_i16_from_f32(&samples, sr, ch)
        }
        other => bail!("unsupported WAV format: {other:?}"),
    };

    if mono_16khz.len() < 16_000 {
        bail!("WAV slice is shorter than 1 second after skipping {start_sec}s");
    }
    Ok(mono_16khz)
}
