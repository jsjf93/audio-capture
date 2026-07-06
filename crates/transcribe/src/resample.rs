//! Sample-rate conversion to Whisper's required 16 kHz.
//!
//! Uses rubato's FFT-based resampler rather than naive linear
//! interpolation: downsampling without a proper low-pass filter aliases
//! high-frequency content down into the speech band, which measurably
//! hurts STT accuracy. Rubato does the filtering correctly and handles
//! non-integer ratios (44.1 kHz → 16 kHz) the same as integer ones.

use rubato::{FftFixedIn, Resampler};

/// Resample mono audio from `from_rate` to `to_rate`. Whole-chunk, not
/// streaming: chunks are short (≤ ~12s), so converting them in one call is
/// simpler than threading resampler state through the pipeline.
///
/// The FFT resampler works on fixed-size blocks and carries a small
/// internal delay, so the output can differ from the mathematically exact
/// length by a few milliseconds of padding — irrelevant for STT.
pub fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }

    const BLOCK: usize = 1024;
    let mut resampler = FftFixedIn::<f32>::new(from_rate as usize, to_rate as usize, BLOCK, 2, 1)
        .expect("resampler construction only fails on zero rates/channels");

    let expected_len = (input.len() as f64 * to_rate as f64 / from_rate as f64) as usize;
    let mut output = Vec::with_capacity(expected_len + BLOCK);

    let mut pos = 0;
    while pos + BLOCK <= input.len() {
        let blocks = resampler
            .process(&[&input[pos..pos + BLOCK]], None)
            .expect("mono block of the fixed size cannot fail");
        output.extend_from_slice(&blocks[0]);
        pos += BLOCK;
    }
    // Final partial block, then flush the resampler's internal delay line so
    // the tail of the audio isn't swallowed.
    let tail: &[f32] = &input[pos..];
    if !tail.is_empty() {
        let blocks = resampler
            .process_partial(Some(&[tail]), None)
            .expect("mono partial block cannot fail");
        output.extend_from_slice(&blocks[0]);
    }
    let blocks = resampler
        .process_partial::<&[f32]>(None, None)
        .expect("flushing cannot fail");
    output.extend_from_slice(&blocks[0]);

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_rates_match() {
        let input = vec![0.5f32; 1000];
        assert_eq!(resample(&input, 16_000, 16_000), input);
    }

    #[test]
    fn downsamples_48k_to_16k_at_correct_length_and_level() {
        // 1 second of a 440 Hz sine at 48 kHz.
        let input: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let out = resample(&input, 48_000, 16_000);

        // Length within a couple of FFT blocks of exact.
        assert!(
            (out.len() as i64 - 16_000).unsigned_abs() < 3000,
            "unexpected output length {}",
            out.len()
        );

        // A 440 Hz tone is far below the new Nyquist (8 kHz), so its energy
        // must survive resampling. Sine RMS = amplitude / sqrt(2) ≈ 0.354.
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(
            (0.25..0.45).contains(&rms),
            "tone energy not preserved: rms={rms}"
        );
    }
}
