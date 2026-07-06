use std::sync::Arc;
use std::time::Instant;

/// Which physical source an [`AudioFrame`] came from.
///
/// This tag is the whole substitute for speaker diarization: we never merge
/// microphone and system-output samples, so every consumer downstream always
/// knows "this is me" vs. "this is everyone else" for free.
///
/// Derives `Serialize` because it crosses the Tauri IPC boundary as part of
/// level events — that's a `serde` concern, not a Tauri one (this crate
/// already depends on `serde` for the tap protocol), so it doesn't violate
/// this crate's "no Tauri dependency" boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Microphone,
    SystemOutput,
}

/// A chunk of interleaved PCM samples captured from one source.
///
/// `samples` is `Arc<[f32]>` rather than `Vec<f32>` deliberately: the bus
/// fans one frame out to potentially several subscribers (a level meter, a
/// health checker, later a transcription client), and `Arc` makes that fan
/// out a cheap pointer clone instead of copying the audio data per
/// subscriber.
#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub source: SourceKind,

    /// Monotonic capture time (`Instant`, not wall-clock), taken as close to
    /// the real-time audio callback as practical. Every source must use this
    /// same convention — monotonic time on its own clock — so that a future
    /// phase can reason about relative ordering between mic and
    /// system-output frames without depending on wall-clock synchronization
    /// across processes.
    pub captured_at: Instant,

    pub sample_rate: u32,
    pub channels: u8,

    /// Interleaved samples, e.g. for stereo: [L, R, L, R, ...].
    pub samples: Arc<[f32]>,
}

impl AudioFrame {
    /// Root-mean-square amplitude across all channels in this frame — a
    /// cheap, standard way to turn a buffer of samples into a single "how
    /// loud is this" number for a level meter.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = self.samples.iter().map(|s| s * s).sum();
        (sum_sq / self.samples.len() as f32).sqrt()
    }
}
