//! The transcription phase's live milestone check: microphone → VAD
//! chunks → local Whisper → console, printed with latency numbers so the
//! "is this fast enough to drive suggestions?" question gets answered with
//! data, not vibes.
//!
//! Usage:
//!   cargo run -p transcribe --example mic_transcribe            # default model
//!   cargo run -p transcribe --example mic_transcribe -- models/ggml-small.en.bin
//!
//! Requires a model file — run `bash scripts/download-model.sh` first.
//! Speak into the mic; each utterance prints when you pause. Ctrl-C stops.

use audio_core::{AudioBus, AudioSource, MicrophoneSource, SourceKind};
use transcribe::{
    resample, ChunkerConfig, SpeechChunk, SpeechChunker, Transcriber, WhisperTranscriber,
    WHISPER_SAMPLE_RATE,
};

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
    let mut transcriber = WhisperTranscriber::new(model_path.as_ref())?;

    // Whisper inference is heavyweight blocking CPU/GPU work; it gets its
    // own OS thread fed through a channel, so the async side (bus consumer)
    // never stalls behind a slow transcription — same discipline as the
    // capture sources' dedicated threads.
    let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<SpeechChunk>();
    let worker = std::thread::spawn(move || {
        while let Ok(chunk) = chunk_rx.recv() {
            let speech_secs = chunk.duration().as_secs_f32();
            let started = std::time::Instant::now();
            let samples_16k = resample(&chunk.samples, chunk.sample_rate, WHISPER_SAMPLE_RATE);
            match transcriber.transcribe(&samples_16k) {
                Ok(text) if !text.is_empty() => {
                    println!(
                        "[you] ({speech_secs:.1}s speech → {:.2}s to transcribe) {text}",
                        started.elapsed().as_secs_f32()
                    );
                }
                Ok(_) => {} // silence/noise chunk that produced no text
                Err(e) => eprintln!("transcription error: {e}"),
            }
        }
    });

    let bus = AudioBus::new(64);
    let mut mic = MicrophoneSource::new();
    mic.start(bus.clone()).await?;
    eprintln!("listening — speak, then pause; Ctrl-C to stop");

    let mut rx = bus.subscribe();
    let mut chunker = SpeechChunker::new(ChunkerConfig::default());

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            frame = rx.recv() => match frame {
                Ok(frame) if frame.source == SourceKind::Microphone => {
                    if let Some(chunk) = chunker.push(&frame.samples, frame.sample_rate, frame.channels) {
                        let _ = chunk_tx.send(chunk);
                    }
                }
                Ok(_) => {} // another source's frames; not ours to transcribe
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    eprintln!("\nstopping …");
    if let Some(chunk) = chunker.flush() {
        let _ = chunk_tx.send(chunk);
    }
    drop(chunk_tx); // lets the worker's recv() loop end
    mic.stop().await?;
    let _ = worker.join();
    Ok(())
}
