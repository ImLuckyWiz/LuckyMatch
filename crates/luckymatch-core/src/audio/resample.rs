/// Shazam fingerprinting expects mono 16 kHz signed 16-bit PCM.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Downmix interleaved `f32` PCM (`[-1.0, 1.0]`) to mono and resample to 16 kHz `i16`.
pub fn to_mono_16khz_i16_from_f32(interleaved: &[f32], sample_rate: u32, channels: u16) -> Vec<i16> {
    if interleaved.is_empty() || sample_rate == 0 || channels == 0 {
        return Vec::new();
    }

    let ch = channels as usize;
    let frame_count = interleaved.len() / ch;
    if frame_count == 0 {
        return Vec::new();
    }

    let mut mono = Vec::with_capacity(frame_count);
    for frame in interleaved.chunks_exact(ch) {
        let sum: f32 = frame.iter().copied().sum();
        mono.push(sum / ch as f32);
    }

    resample_mono_f32_to_16khz_i16(&mono, sample_rate)
}

/// Downmix interleaved `i16` PCM to mono and resample to 16 kHz `i16`.
pub fn to_mono_16khz_i16_from_i16(interleaved: &[i16], sample_rate: u32, channels: u16) -> Vec<i16> {
    if interleaved.is_empty() || sample_rate == 0 || channels == 0 {
        return Vec::new();
    }

    let ch = channels as usize;
    let frame_count = interleaved.len() / ch;
    if frame_count == 0 {
        return Vec::new();
    }

    if sample_rate == TARGET_SAMPLE_RATE && ch == 1 {
        return interleaved.to_vec();
    }

    let mut mono = Vec::with_capacity(frame_count);
    for frame in interleaved.chunks_exact(ch) {
        let sum: i32 = frame.iter().map(|&s| s as i32).sum();
        mono.push((sum as f32 / ch as f32) / 32768.0);
    }

    resample_mono_f32_to_16khz_i16(&mono, sample_rate)
}

fn resample_mono_f32_to_16khz_i16(mono: &[f32], sample_rate: u32) -> Vec<i16> {
    if mono.is_empty() || sample_rate == 0 {
        return Vec::new();
    }

    if sample_rate == TARGET_SAMPLE_RATE {
        return mono.iter().copied().map(float_to_i16).collect();
    }

    let out_len = ((mono.len() as u64 * TARGET_SAMPLE_RATE as u64) / sample_rate as u64) as usize;
    if out_len == 0 {
        return Vec::new();
    }

    let ratio = sample_rate as f64 / TARGET_SAMPLE_RATE as f64;
    let mut out = Vec::with_capacity(out_len);

    if sample_rate > TARGET_SAMPLE_RATE {
        // Windowed/boxcar average over each output step's source span to avoid aliasing.
        for i in 0..out_len {
            let start_f = i as f64 * ratio;
            let end_f = ((i + 1) as f64 * ratio).min(mono.len() as f64);
            let start_idx = (start_f.floor() as usize).min(mono.len() - 1);
            let end_idx = (end_f.ceil() as usize).clamp(start_idx + 1, mono.len());

            let mut acc = 0.0f64;
            for &s in &mono[start_idx..end_idx] {
                acc += s as f64;
            }
            let avg = (acc / (end_idx - start_idx) as f64) as f32;
            out.push(float_to_i16(avg));
        }
    } else {
        // Linear interpolation when upsampling.
        for i in 0..out_len {
            let src = i as f64 * ratio;
            let idx = (src.floor() as usize).min(mono.len() - 1);
            let next = (idx + 1).min(mono.len() - 1);
            let frac = (src - idx as f64) as f32;
            let sample = mono[idx] * (1.0 - frac) + mono[next] * frac;
            out.push(float_to_i16(sample));
        }
    }

    out
}

#[inline]
fn float_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * 32767.0).round() as i16
}

/// Compute normalized root-mean-square signal level in `[0.0, 1.0]`.
pub fn rms_level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let v = s as f64 / 32768.0;
            v * v
        })
        .sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsamples_48khz_stereo_f32_to_16khz_mono_i16() {
        let sr = 48_000;
        let frames = sr; // 1 second
        let mut stereo = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / sr as f32;
            let val = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            stereo.push(val);
            stereo.push(val);
        }

        let out = to_mono_16khz_i16_from_f32(&stereo, sr as u32, 2);
        assert_eq!(out.len(), 16_000);
        let rms = rms_level(&out);
        assert!(rms > 0.25 && rms < 0.45, "unexpected rms: {rms}");
    }

    #[test]
    fn downsamples_44100_i16_to_16khz() {
        let sr = 44_100;
        let samples = vec![10_000i16; sr * 2]; // 1s stereo
        let out = to_mono_16khz_i16_from_i16(&samples, sr as u32, 2);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|&s| (s - 10_000).abs() <= 5));
    }
}
