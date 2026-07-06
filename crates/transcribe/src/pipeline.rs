//! Glue between the two buses: AudioBus frames in, TranscriptSegments out.
//!
//! Each source gets its *own* `SpeechChunker` — utterance boundaries in
//! the user's speech and in system audio are completely independent, and
//! sharing a chunker would let one stream's silence close the other
//! stream's chunk. Inference, by contrast, is *shared*: one Whisper
//! context on one worker thread serves both streams. A second context
//! would double model memory (hundreds of MB) to parallelize work that
//! runs at a few percent of real time — queueing is the better trade
//! until measurements say otherwise.

use crate::{
    resample, ChunkerConfig, SpeechChunk, SpeechChunker, Transcriber, TranscriptBus,
    TranscriptSegment, WHISPER_SAMPLE_RATE,
};
use audio_core::{AudioBus, SourceKind};

/// Consumes `audio_bus` until it closes (every source dropped), publishing
/// transcript segments to `transcript_bus`. Spawn it as a task; dropping
/// or aborting that task tears everything down, including the worker
/// thread (its channel sender lives in this future).
pub async fn run_transcription(
    audio_bus: AudioBus,
    transcript_bus: TranscriptBus,
    transcriber: Box<dyn Transcriber>,
    chunker_config: ChunkerConfig,
) {
    let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<(SourceKind, SpeechChunk)>();

    // Whisper inference is heavyweight blocking CPU/GPU work; it lives on
    // its own OS thread so the bus consumer below never stalls behind it —
    // the same discipline the capture sources use for their real-time work.
    let worker = std::thread::Builder::new()
        .name("transcription-worker".into())
        .spawn(move || {
            let mut transcriber = transcriber;
            while let Ok((source, chunk)) = chunk_rx.recv() {
                let speech_duration = chunk.duration();
                let samples_16k = resample(&chunk.samples, chunk.sample_rate, WHISPER_SAMPLE_RATE);
                match transcriber.transcribe(&samples_16k) {
                    Ok(text) if !text.is_empty() => transcript_bus.publish(TranscriptSegment {
                        source,
                        text,
                        speech_duration,
                    }),
                    Ok(_) => {} // chunk produced no text (noise); nothing to publish
                    Err(e) => eprintln!("[transcription:{}] error: {e}", label(source)),
                }
            }
        })
        .expect("failed to spawn transcription-worker thread");

    let mut rx = audio_bus.subscribe();
    let mut mic_chunker = SpeechChunker::new(chunker_config.clone());
    let mut system_chunker = SpeechChunker::new(chunker_config);

    loop {
        match rx.recv().await {
            Ok(frame) => {
                let chunker = match frame.source {
                    SourceKind::Microphone => &mut mic_chunker,
                    SourceKind::SystemOutput => &mut system_chunker,
                };
                if let Some(chunk) = chunker.push(&frame.samples, frame.sample_rate, frame.channels)
                {
                    let _ = chunk_tx.send((frame.source, chunk));
                }
            }
            // Lagging drops frames mid-utterance; the chunker just sees a
            // gap and the transcript loses a moment of audio. Acceptable —
            // the bus contract is best-effort, and blocking here would
            // stall every other consumer's producer instead.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }

    // Bus closed: flush in-flight speech so trailing words aren't lost.
    if let Some(chunk) = mic_chunker.flush() {
        let _ = chunk_tx.send((SourceKind::Microphone, chunk));
    }
    if let Some(chunk) = system_chunker.flush() {
        let _ = chunk_tx.send((SourceKind::SystemOutput, chunk));
    }
    drop(chunk_tx); // ends the worker's recv() loop
    // Blocking join is fine here: this runs once, at pipeline teardown.
    let _ = worker.join();
}

fn label(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Microphone => "mic",
        SourceKind::SystemOutput => "system",
    }
}
