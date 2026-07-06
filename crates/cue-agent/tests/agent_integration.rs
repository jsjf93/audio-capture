//! CI-runnable proof of the agent loop, with a fake model standing in for
//! the API: transcript segments in → correctly-tagged suggestions out,
//! trigger gates respected, and the model's "pass" path producing nothing.

use audio_core::SourceKind;
use cue_agent::{
    run_cue_agent, AgentError, ModelSuggestion, Suggestion, SuggestionBus, SuggestionModel,
    TriggerConfig,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use transcribe::{TranscriptBus, TranscriptSegment};

/// Suggests on every call, echoing how much context it saw.
struct AlwaysSuggest {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl SuggestionModel for AlwaysSuggest {
    async fn suggest(
        &self,
        transcript: &str,
        _recent_hints: &[String],
    ) -> Result<Option<ModelSuggestion>, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(ModelSuggestion {
            cue: "test cue".into(),
            hint: format!("saw {} chars", transcript.len()),
            detail: "detail".into(),
        }))
    }
}

/// Never suggests — the model's "pass" path.
struct NeverSuggest;

#[async_trait::async_trait]
impl SuggestionModel for NeverSuggest {
    async fn suggest(
        &self,
        _transcript: &str,
        _recent_hints: &[String],
    ) -> Result<Option<ModelSuggestion>, AgentError> {
        Ok(None)
    }
}

fn segment(source: SourceKind, text: &str) -> TranscriptSegment {
    TranscriptSegment {
        source,
        text: text.to_string(),
        speech_duration: Duration::from_secs(2),
    }
}

fn test_config() -> TriggerConfig {
    TriggerConfig {
        cooldown: Duration::from_millis(10),
        min_new_words: 3,
        context_char_budget: 1_000,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn suggestions_carry_the_triggering_segments_source() {
    let transcript_bus = TranscriptBus::new(16);
    let suggestion_bus = SuggestionBus::new(16);
    let mut rx = suggestion_bus.subscribe();

    let agent = tokio::spawn(run_cue_agent(
        transcript_bus.clone(),
        suggestion_bus.clone(),
        Arc::new(AlwaysSuggest { calls: AtomicUsize::new(0) }),
        test_config(),
    ));

    transcript_bus.publish(segment(
        SourceKind::SystemOutput,
        "we would need budget approval before moving forward",
    ));

    let suggestion: Suggestion = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for a suggestion")
        .expect("suggestion bus closed");
    assert_eq!(suggestion.source, SourceKind::SystemOutput);
    assert!(suggestion.hint.starts_with("saw "));

    agent.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn short_segments_do_not_trigger_a_model_call() {
    let transcript_bus = TranscriptBus::new(16);
    let suggestion_bus = SuggestionBus::new(16);
    let mut rx = suggestion_bus.subscribe();

    let agent = tokio::spawn(run_cue_agent(
        transcript_bus.clone(),
        suggestion_bus.clone(),
        Arc::new(AlwaysSuggest { calls: AtomicUsize::new(0) }),
        test_config(),
    ));

    transcript_bus.publish(segment(SourceKind::Microphone, "ok"));
    transcript_bus.publish(segment(SourceKind::SystemOutput, "yeah"));

    // Nothing should arrive: 2 words < min_new_words. A short wait keeps
    // this honest without making the suite slow.
    let result = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(result.is_err(), "no suggestion should be produced for two filler words");

    agent.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passing_model_produces_no_suggestions() {
    let transcript_bus = TranscriptBus::new(16);
    let suggestion_bus = SuggestionBus::new(16);
    let mut rx = suggestion_bus.subscribe();

    let agent = tokio::spawn(run_cue_agent(
        transcript_bus.clone(),
        suggestion_bus.clone(),
        Arc::new(NeverSuggest),
        test_config(),
    ));

    transcript_bus.publish(segment(
        SourceKind::SystemOutput,
        "we would need budget approval before moving forward with this project",
    ));

    let result = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(result.is_err(), "a passing model must not publish anything");

    agent.abort();
}
