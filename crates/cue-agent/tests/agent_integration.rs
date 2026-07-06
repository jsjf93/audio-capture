//! CI-runnable proof of the agent loop, with fake models standing in for
//! the API: transcript segments in → correctly-tagged suggestions out,
//! trigger gates respected, the model's "pass" path producing nothing,
//! and notes flowing back into subsequent calls (the memory contract).

use audio_core::SourceKind;
use cue_agent::{
    run_cue_agent, AgentError, ModelOutcome, ModelSuggestion, Suggestion, SuggestionBus,
    SuggestionModel, TriggerConfig,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
        _notes: &str,
        _recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelOutcome {
            suggestion: Some(ModelSuggestion {
                cue: "test cue".into(),
                hint: format!("saw {} chars", transcript.len()),
                detail: "detail".into(),
                value: 10,
            }),
            updated_notes: None,
        })
    }
}

/// Never suggests — the model's "pass" path.
struct NeverSuggest;

#[async_trait::async_trait]
impl SuggestionModel for NeverSuggest {
    async fn suggest(
        &self,
        _transcript: &str,
        _notes: &str,
        _recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError> {
        Ok(ModelOutcome::default())
    }
}

/// Records the notes it receives and always writes new ones, so the test
/// can assert the agent loop feeds each call the previous call's notes.
struct NoteTaker {
    seen_notes: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl SuggestionModel for NoteTaker {
    async fn suggest(
        &self,
        _transcript: &str,
        notes: &str,
        _recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError> {
        let mut seen = self.seen_notes.lock().unwrap();
        seen.push(notes.to_string());
        let version = seen.len();
        // Hints must be genuinely distinct or the repeat-suppression gate
        // (correctly) swallows the second one.
        let hint = match version {
            1 => "Ask who owns the budget approval",
            _ => "Anchor a specific follow-up meeting time",
        };
        Ok(ModelOutcome {
            suggestion: Some(ModelSuggestion {
                cue: "cue".into(),
                hint: hint.into(),
                detail: "detail".into(),
                value: 10,
            }),
            updated_notes: Some(format!("notes v{version}")),
        })
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

/// The agent subscribes to the transcript bus *inside* its spawned task,
/// and a broadcast channel only delivers to existing subscribers — so a
/// publish racing the spawn would vanish. Give the task a beat to
/// subscribe before publishing. (Not an issue in the real app, where
/// segments flow continuously.)
async fn let_agent_subscribe() {
    tokio::time::sleep(Duration::from_millis(100)).await;
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
        1,
    ));

    let_agent_subscribe().await;
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
        1,
    ));

    let_agent_subscribe().await;
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
        1,
    ));

    let_agent_subscribe().await;
    transcript_bus.publish(segment(
        SourceKind::SystemOutput,
        "we would need budget approval before moving forward with this project",
    ));

    let result = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(result.is_err(), "a passing model must not publish anything");

    agent.abort();
}

/// Re-proposes the same suggestion in different words every call —
/// modeling the real failure where a persistent situation makes the model
/// keep suggesting its favorite fix past the "don't repeat" instruction.
struct StuckOnARepeat {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl SuggestionModel for StuckOnARepeat {
    async fn suggest(
        &self,
        _transcript: &str,
        _notes: &str,
        _recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let hint = if n == 0 {
            "Assign owner and sprint deadline to each of the four requirements now"
        } else {
            "Assign owners and deadlines to the four requirements before the meeting ends"
        };
        Ok(ModelOutcome {
            suggestion: Some(ModelSuggestion {
                cue: "four requirements".into(),
                hint: hint.into(),
                detail: "detail".into(),
                value: 9,
            }),
            updated_notes: None,
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reworded_repeats_are_suppressed() {
    let transcript_bus = TranscriptBus::new(16);
    let suggestion_bus = SuggestionBus::new(16);
    let mut rx = suggestion_bus.subscribe();

    let agent = tokio::spawn(run_cue_agent(
        transcript_bus.clone(),
        suggestion_bus.clone(),
        Arc::new(StuckOnARepeat { calls: AtomicUsize::new(0) }),
        test_config(),
        1,
    ));

    let_agent_subscribe().await;
    transcript_bus.publish(segment(
        SourceKind::SystemOutput,
        "so those are the four requirements we need to get through this sprint",
    ));
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("bus closed");
    assert!(first.hint.contains("Assign owner"));

    tokio::time::sleep(Duration::from_millis(30)).await;
    transcript_bus.publish(segment(
        SourceKind::Microphone,
        "right okay let us keep talking through the details of the plan",
    ));

    let result = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(
        result.is_err(),
        "the reworded repeat must be suppressed app-side"
    );

    agent.abort();
}

/// Always proposes, but scores low — the app-side gate must filter it.
struct LowValueSuggest;

#[async_trait::async_trait]
impl SuggestionModel for LowValueSuggest {
    async fn suggest(
        &self,
        _transcript: &str,
        _notes: &str,
        _recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError> {
        Ok(ModelOutcome {
            suggestion: Some(ModelSuggestion {
                cue: "cue".into(),
                hint: "a marginal observation".into(),
                detail: "detail".into(),
                value: 2,
            }),
            updated_notes: None,
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn candidates_below_the_mode_threshold_are_not_published() {
    let transcript_bus = TranscriptBus::new(16);
    let suggestion_bus = SuggestionBus::new(16);
    let mut rx = suggestion_bus.subscribe();

    let agent = tokio::spawn(run_cue_agent(
        transcript_bus.clone(),
        suggestion_bus.clone(),
        Arc::new(LowValueSuggest),
        test_config(),
        5, // threshold above the fake's value of 2
    ));

    let_agent_subscribe().await;
    transcript_bus.publish(segment(
        SourceKind::SystemOutput,
        "we would need budget approval before moving forward with this project",
    ));

    let result = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(
        result.is_err(),
        "a value-2 candidate must not clear a threshold of 5"
    );

    agent.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_from_one_call_are_fed_into_the_next() {
    let transcript_bus = TranscriptBus::new(16);
    let suggestion_bus = SuggestionBus::new(16);
    let mut rx = suggestion_bus.subscribe();

    let model = Arc::new(NoteTaker {
        seen_notes: Mutex::new(Vec::new()),
    });
    let agent = tokio::spawn(run_cue_agent(
        transcript_bus.clone(),
        suggestion_bus.clone(),
        model.clone(),
        test_config(),
        1,
    ));

    let_agent_subscribe().await;
    transcript_bus.publish(segment(
        SourceKind::SystemOutput,
        "the prospect said budget approval rests with their finance director",
    ));
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("bus closed");
    assert_eq!(first.hint, "Ask who owns the budget approval");

    // Wait out the cooldown, then trigger a second call.
    tokio::time::sleep(Duration::from_millis(30)).await;
    transcript_bus.publish(segment(
        SourceKind::Microphone,
        "understood so I will follow up with the finance director tomorrow",
    ));
    let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("bus closed");
    assert_eq!(second.hint, "Anchor a specific follow-up meeting time");

    let seen = model.seen_notes.lock().unwrap();
    assert_eq!(seen[0], "", "first call starts with empty notes");
    assert_eq!(
        seen[1], "notes v1",
        "second call must receive the notes written by the first"
    );

    agent.abort();
}
