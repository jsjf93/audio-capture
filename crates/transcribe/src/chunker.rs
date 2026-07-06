//! Energy-based voice activity detection and chunking.
//!
//! Whisper isn't a streaming model — it transcribes discrete clips — so
//! something has to decide where one utterance ends and the next begins.
//! This is the simplest thing that works: a frame whose RMS crosses a
//! threshold is "speech", and a chunk closes after a run of silence (the
//! hangover) or when it hits a maximum length. A small pre-roll of audio
//! from just *before* the threshold crossing is prepended so quiet
//! utterance onsets ("so...", "well...") don't get clipped.
//!
//! Known limitation, on purpose: a fixed RMS threshold is crude compared to
//! a trained VAD (e.g. Silero). It's good enough to prove the pipeline and
//! is isolated here so a smarter detector can replace it without touching
//! anything else.

use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Frame RMS at or above this counts as speech. For reference: a quiet
    /// room floor measures ~0.001 on typical hardware; normal speech into a
    /// nearby mic lands around 0.02–0.15.
    pub speech_rms_threshold: f32,
    /// How much continuous silence ends a chunk. Long enough to survive
    /// mid-sentence pauses, short enough that the transcript feels live.
    pub silence_hangover: Duration,
    /// Chunks whose speech content is shorter than this are discarded as
    /// noise (a cough, a keyboard clack).
    pub min_speech: Duration,
    /// Hard cap per chunk, so a monologue still produces incremental
    /// transcript instead of one giant deferred blob.
    pub max_chunk: Duration,
    /// Audio kept from before the threshold crossing and prepended to the
    /// chunk, so quiet utterance onsets aren't clipped.
    pub pre_roll: Duration,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            speech_rms_threshold: 0.01,
            silence_hangover: Duration::from_millis(700),
            min_speech: Duration::from_millis(300),
            max_chunk: Duration::from_secs(12),
            pre_roll: Duration::from_millis(250),
        }
    }
}

/// A finished utterance: mono samples at the capture rate (resampling to
/// Whisper's 16 kHz happens later, in one place).
#[derive(Debug, Clone)]
pub struct SpeechChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl SpeechChunk {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / self.sample_rate as f64)
    }
}

struct ActiveChunk {
    samples: Vec<f32>,
    /// Length of the current trailing run of silent samples — reset to zero
    /// every time a speech frame arrives.
    trailing_silence: usize,
    /// Total above-threshold samples — the real "speech content" measure
    /// used for the min_speech check (buffer length would overcount, since
    /// it includes pre-roll and hangover silence).
    speech_samples: usize,
}

pub struct SpeechChunker {
    config: ChunkerConfig,
    sample_rate: Option<u32>,
    pre_roll: VecDeque<f32>,
    active: Option<ActiveChunk>,
}

impl SpeechChunker {
    pub fn new(config: ChunkerConfig) -> Self {
        Self {
            config,
            sample_rate: None,
            pre_roll: VecDeque::new(),
            active: None,
        }
    }

    /// Feed one frame of interleaved samples; returns a finished chunk when
    /// an utterance ends (or hits the max length) on this frame.
    pub fn push(&mut self, samples: &[f32], sample_rate: u32, channels: u8) -> Option<SpeechChunk> {
        let mono = downmix(samples, channels);

        // A sample-rate change mid-stream means the capture device changed;
        // mixing rates inside one chunk would produce garbage audio, so
        // abandon in-flight state and start clean at the new rate.
        if self.sample_rate != Some(sample_rate) {
            self.sample_rate = Some(sample_rate);
            self.pre_roll.clear();
            self.active = None;
        }

        let speaking = rms(&mono) >= self.config.speech_rms_threshold;

        // Computed before the `match` borrows `self.active` mutably.
        let pre_roll_cap = self.duration_in_samples(self.config.pre_roll);
        let hangover_samples = self.duration_in_samples(self.config.silence_hangover);
        let max_chunk_samples = self.duration_in_samples(self.config.max_chunk);

        match &mut self.active {
            None if speaking => {
                let mut chunk_samples: Vec<f32> = self.pre_roll.drain(..).collect();
                chunk_samples.extend_from_slice(&mono);
                self.active = Some(ActiveChunk {
                    samples: chunk_samples,
                    trailing_silence: 0,
                    speech_samples: mono.len(),
                });
                None
            }
            None => {
                self.pre_roll.extend(mono.iter().copied());
                while self.pre_roll.len() > pre_roll_cap {
                    self.pre_roll.pop_front();
                }
                None
            }
            Some(active) => {
                active.samples.extend_from_slice(&mono);
                if speaking {
                    active.trailing_silence = 0;
                    active.speech_samples += mono.len();
                } else {
                    active.trailing_silence += mono.len();
                }

                let ended = active.trailing_silence >= hangover_samples;
                let full = active.samples.len() >= max_chunk_samples;
                if ended || full {
                    self.finalize()
                } else {
                    None
                }
            }
        }
    }

    /// Close out any in-flight chunk (stream ending, capture stopped).
    pub fn flush(&mut self) -> Option<SpeechChunk> {
        self.finalize()
    }

    fn finalize(&mut self) -> Option<SpeechChunk> {
        let active = self.active.take()?;
        let sample_rate = self.sample_rate?;
        if active.speech_samples < self.duration_in_samples(self.config.min_speech) {
            return None;
        }
        Some(SpeechChunk {
            samples: active.samples,
            sample_rate,
        })
    }

    fn duration_in_samples(&self, d: Duration) -> usize {
        let rate = self.sample_rate.unwrap_or(0) as f64;
        (d.as_secs_f64() * rate) as usize
    }
}

/// Average interleaved channels down to mono. Whisper expects mono, and
/// nothing downstream cares about stereo separation within one source.
fn downmix(samples: &[f32], channels: u8) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    if channels == 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    const FRAME: usize = 320; // 20ms at 16kHz, matching real capture frames

    fn speech_frame() -> Vec<f32> {
        // Constant 0.1 amplitude → RMS 0.1, comfortably above threshold.
        vec![0.1; FRAME]
    }

    fn silent_frame() -> Vec<f32> {
        vec![0.0; FRAME]
    }

    fn push_seconds(
        chunker: &mut SpeechChunker,
        seconds: f32,
        frame: &[f32],
        out: &mut Vec<SpeechChunk>,
    ) {
        let frames = (seconds * RATE as f32 / FRAME as f32) as usize;
        for _ in 0..frames {
            if let Some(chunk) = chunker.push(frame, RATE, 1) {
                out.push(chunk);
            }
        }
    }

    #[test]
    fn one_utterance_produces_one_chunk() {
        let mut chunker = SpeechChunker::new(ChunkerConfig::default());
        let mut chunks = Vec::new();
        push_seconds(&mut chunker, 0.5, &silent_frame(), &mut chunks);
        push_seconds(&mut chunker, 2.0, &speech_frame(), &mut chunks);
        push_seconds(&mut chunker, 1.5, &silent_frame(), &mut chunks);

        assert_eq!(chunks.len(), 1);
        let dur = chunks[0].duration().as_secs_f32();
        // ~250ms pre-roll + 2s speech + ~700ms hangover
        assert!(
            (2.5..3.5).contains(&dur),
            "unexpected chunk duration {dur}s"
        );
    }

    #[test]
    fn short_blip_is_discarded() {
        let mut chunker = SpeechChunker::new(ChunkerConfig::default());
        let mut chunks = Vec::new();
        push_seconds(&mut chunker, 0.5, &silent_frame(), &mut chunks);
        push_seconds(&mut chunker, 0.1, &speech_frame(), &mut chunks); // a click/cough
        push_seconds(&mut chunker, 1.5, &silent_frame(), &mut chunks);

        assert!(chunks.is_empty(), "sub-min_speech blip should be dropped");
    }

    #[test]
    fn long_speech_is_split_at_max_chunk() {
        let mut chunker = SpeechChunker::new(ChunkerConfig::default());
        let mut chunks = Vec::new();
        push_seconds(&mut chunker, 15.0, &speech_frame(), &mut chunks);
        assert_eq!(chunks.len(), 1, "max_chunk should force an emit mid-speech");
        assert!((11.5..12.5).contains(&chunks[0].duration().as_secs_f32()));

        // The remainder is still in flight and comes out on flush.
        let tail = chunker.flush().expect("remaining speech should flush");
        assert!(tail.duration().as_secs_f32() > 2.0);
    }

    #[test]
    fn mid_sentence_pause_does_not_split() {
        let mut chunker = SpeechChunker::new(ChunkerConfig::default());
        let mut chunks = Vec::new();
        push_seconds(&mut chunker, 1.0, &speech_frame(), &mut chunks);
        push_seconds(&mut chunker, 0.4, &silent_frame(), &mut chunks); // < hangover
        push_seconds(&mut chunker, 1.0, &speech_frame(), &mut chunks);
        push_seconds(&mut chunker, 1.0, &silent_frame(), &mut chunks);

        assert_eq!(chunks.len(), 1, "a 400ms pause should stay inside one chunk");
    }

    #[test]
    fn sample_rate_change_resets_cleanly() {
        let mut chunker = SpeechChunker::new(ChunkerConfig::default());
        let mut chunks = Vec::new();
        push_seconds(&mut chunker, 1.0, &speech_frame(), &mut chunks);
        // Device switch mid-utterance: new rate arrives.
        assert!(chunker.push(&speech_frame(), 48_000, 1).is_none());
        assert!(chunks.is_empty(), "in-flight mixed-rate chunk must be dropped");
    }
}
