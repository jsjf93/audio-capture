//! Local Whisper inference via whisper-rs (bindings to whisper.cpp).
//!
//! Chosen over cloud STT for the first pipeline deliberately: zero cost,
//! nothing leaves the machine (call audio is about as sensitive as data
//! gets), works offline, and it forced the chunking/resampling machinery
//! into existence — which any future cloud implementation of `Transcriber`
//! reuses unchanged.

use crate::{TranscribeError, Transcriber, WHISPER_SAMPLE_RATE};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperTranscriber {
    ctx: WhisperContext,
}

impl WhisperTranscriber {
    /// Loads a ggml/gguf Whisper model (see `scripts/download-model.sh`).
    /// Loading takes a few seconds and a few hundred MB of memory — do it
    /// once and keep the instance around, not per chunk.
    pub fn new(model_path: &Path) -> Result<Self, TranscribeError> {
        // whisper.cpp logs copiously to stderr/stdout by default (model
        // metadata, per-chunk decoder state). Route it into the `log`
        // facade instead; without a registered logger that means silence,
        // and any future logger we do register will capture it properly.
        // Safe to call repeatedly — it installs once.
        whisper_rs::install_logging_hooks();

        let path = model_path
            .to_str()
            .ok_or_else(|| TranscribeError::ModelLoad("model path is not valid UTF-8".into()))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| TranscribeError::ModelLoad(e.to_string()))?;
        Ok(Self { ctx })
    }
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(&mut self, samples: &[f32]) -> Result<String, TranscribeError> {
        // whisper.cpp rejects clips shorter than ~1s; pad with trailing
        // silence rather than making every caller care about the rule.
        let min_len = (WHISPER_SAMPLE_RATE as usize * 11) / 10; // 1.1s
        let padded;
        let samples: &[f32] = if samples.len() < min_len {
            padded = {
                let mut v = samples.to_vec();
                v.resize(min_len, 0.0);
                v
            };
            &padded
        } else {
            samples
        };

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // English-only for now — matches the *.en models the download
        // script fetches. Becomes a config knob when multilingual matters.
        params.set_language(Some("en"));
        // whisper.cpp prints its own progress/results to stdout by default;
        // we are the ones consuming the text, so silence all of it.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // Each chunk is an independent utterance: don't let the previous
        // chunk's text bias this one (context carryover helps long-form
        // dictation but compounds errors across a live conversation).
        params.set_no_context(true);

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| TranscribeError::Inference(e.to_string()))?;
        state
            .full(params, samples)
            .map_err(|e| TranscribeError::Inference(e.to_string()))?;

        let mut text = String::new();
        for segment in state.as_iter() {
            let piece = segment
                .to_str_lossy()
                .map_err(|e| TranscribeError::Inference(e.to_string()))?;
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(piece);
        }
        Ok(text)
    }
}
