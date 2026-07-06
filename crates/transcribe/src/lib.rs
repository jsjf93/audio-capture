//! The transcription stage of the pipeline: consumes `AudioFrame`s from an
//! `AudioBus`, groups them into speech chunks with a simple energy-based
//! VAD (`SpeechChunker`), and turns each chunk into text through a
//! `Transcriber` implementation.
//!
//! Deliberately Tauri-free, like `audio-core`: this crate must stay
//! buildable and testable standalone, so the eventual engine/thin-client
//! split costs nothing. It never touches capture internals either — it is
//! just another bus subscriber, which is the whole point of the bus.

mod bus;
mod chunker;
mod pipeline;
mod resample;
mod whisper;

pub use bus::TranscriptBus;
pub use chunker::{ChunkerConfig, SpeechChunk, SpeechChunker};
pub use pipeline::run_transcription;
pub use resample::resample;
pub use whisper::WhisperTranscriber;

use audio_core::SourceKind;

/// Whisper models are trained on 16 kHz mono audio; every chunk gets
/// resampled to this before inference regardless of its capture rate.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("failed to load whisper model: {0}")]
    ModelLoad(String),
    #[error("transcription failed: {0}")]
    Inference(String),
}

/// Anything that can turn a chunk of 16 kHz mono samples into text.
///
/// The trait exists so a cloud streaming STT implementation can slot in
/// later without touching the chunking/resampling machinery — the same
/// move `AudioSource` makes on the capture side.
pub trait Transcriber: Send {
    /// `samples` must be 16 kHz mono f32 (see [`WHISPER_SAMPLE_RATE`]).
    fn transcribe(&mut self, samples: &[f32]) -> Result<String, TranscribeError>;
}

/// One finished piece of transcript. Carries the same `SourceKind` tag as
/// the audio it came from — the "you vs. everyone else" separation must
/// survive every pipeline stage, and this is where it crosses from audio
/// into text.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptSegment {
    pub source: SourceKind,
    pub text: String,
    /// Duration of the speech audio this text came from.
    pub speech_duration: std::time::Duration,
}
