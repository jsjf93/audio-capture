//! CI-runnable proof that the transcription pipeline preserves per-source
//! separation: two fake sources publish simultaneously onto one AudioBus,
//! and every TranscriptSegment that comes out the other end must carry the
//! right SourceKind tag. The audio-level no-bleed guarantee is already
//! covered by audio-core's bus_integration tests; this covers the text
//! stage on top of it, with a fake Transcriber so no model is needed.

use audio_core::{AudioBus, AudioSource, FakeAudioSource, SourceKind};
use std::time::Duration;
use transcribe::{
    run_transcription, ChunkerConfig, TranscribeError, Transcriber, TranscriptBus,
};

/// Echoes the chunk's sample count instead of running a model — enough to
/// verify chunks flow through tagged and resampled.
struct FakeTranscriber;

impl Transcriber for FakeTranscriber {
    fn transcribe(&mut self, samples: &[f32]) -> Result<String, TranscribeError> {
        Ok(format!("chunk of {} samples", samples.len()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn both_sources_flow_through_tagged_independently() {
    let audio_bus = AudioBus::new(256);
    let transcript_bus = TranscriptBus::new(64);
    let mut transcript_rx = transcript_bus.subscribe();

    // Constant-amplitude "speech" never pauses, so chunks are emitted via
    // the max_chunk split — shrunk here so the test runs in milliseconds.
    let config = ChunkerConfig {
        speech_rms_threshold: 0.01,
        silence_hangover: Duration::from_millis(50),
        min_speech: Duration::ZERO,
        max_chunk: Duration::from_millis(200),
        pre_roll: Duration::ZERO,
    };

    let pipeline = tokio::spawn(run_transcription(
        audio_bus.clone(),
        transcript_bus.clone(),
        Box::new(FakeTranscriber),
        config,
    ));

    // Different sample rates on purpose: the system-output path exercises
    // the 48k→16k resample inside the pipeline, mirroring real capture.
    let mut mic = FakeAudioSource::new(SourceKind::Microphone, vec![0.25; 3200], 16_000, 1)
        .with_frame_interval(Duration::from_millis(1));
    let mut system = FakeAudioSource::new(SourceKind::SystemOutput, vec![0.25; 9600], 48_000, 1)
        .with_frame_interval(Duration::from_millis(1));
    mic.start(audio_bus.clone()).await.unwrap();
    system.start(audio_bus.clone()).await.unwrap();

    let mut mic_segments = 0u32;
    let mut system_segments = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while mic_segments < 2 || system_segments < 2 {
        let segment = tokio::time::timeout_at(deadline, transcript_rx.recv())
            .await
            .expect("timed out waiting for transcript segments")
            .expect("transcript bus closed unexpectedly");

        assert!(
            segment.text.contains("samples"),
            "segment should come from the FakeTranscriber, got: {}",
            segment.text
        );
        assert!(segment.speech_duration > Duration::ZERO);
        match segment.source {
            SourceKind::Microphone => mic_segments += 1,
            SourceKind::SystemOutput => system_segments += 1,
        }
    }

    mic.stop().await.unwrap();
    system.stop().await.unwrap();
    pipeline.abort();
}
