//! The dual-stream transcription milestone: microphone *and* system output
//! transcribed live, side by side, each line tagged with who it came from.
//! This is the text-level equivalent of audio-core's `dual_capture` proof.
//!
//! Usage:
//!   cargo run -p transcribe --example dual_transcribe            # default model
//!   cargo run -p transcribe --example dual_transcribe -- models/ggml-small.en.bin
//!
//! Needs a model (`bash scripts/download-model.sh`) and the Swift helper
//! built (`swift build` in swift-helper/). Speak while playing a video —
//! your words should print as [you], the video's as [them], never mixed.

use audio_core::{AudioBus, AudioSource, MicrophoneSource, SourceKind, SystemOutputSource};
use transcribe::{run_transcription, ChunkerConfig, TranscriptBus, WhisperTranscriber};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/ggml-base.en.bin".to_string());
    if !std::path::Path::new(&model_path).exists() {
        eprintln!("model not found at `{model_path}`");
        eprintln!("run: bash scripts/download-model.sh   (or pass a model path as the first arg)");
        std::process::exit(1);
    }

    eprintln!("loading whisper model from {model_path} …");
    let transcriber = WhisperTranscriber::new(model_path.as_ref())?;

    let audio_bus = AudioBus::new(256);
    let transcript_bus = TranscriptBus::new(64);
    let mut transcript_rx = transcript_bus.subscribe();

    tokio::spawn(run_transcription(
        audio_bus.clone(),
        transcript_bus.clone(),
        Box::new(transcriber),
        ChunkerConfig::default(),
    ));

    let mut mic = MicrophoneSource::new();
    let mut system = SystemOutputSource::new();
    mic.start(audio_bus.clone()).await?;
    system.start(audio_bus.clone()).await?;
    eprintln!("listening on both streams — speak and/or play audio; Ctrl-C to stop");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            segment = transcript_rx.recv() => match segment {
                Ok(segment) => {
                    let who = match segment.source {
                        SourceKind::Microphone => "you",
                        SourceKind::SystemOutput => "them",
                    };
                    println!(
                        "[{who}] ({:.1}s) {}",
                        segment.speech_duration.as_secs_f32(),
                        segment.text
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    eprintln!("\nstopping …");
    mic.stop().await?;
    system.stop().await?;
    Ok(())
}
