//! The Tier-1 cue agent: the always-on, cheap-model stage that watches the
//! conversation and proposes at most one timely suggestion at a time.
//!
//! Pipeline position: `TranscriptBus` in → `SuggestionBus` out. Like every
//! other stage, it's just a bus subscriber — transcription has no idea it
//! exists, and whoever consumes suggestions (today: src-tauri forwarding
//! to the overlay window) has no idea an LLM is involved.
//!
//! Tauri-free by design, same as audio-core and transcribe.

mod anthropic;
mod trigger;

pub use anthropic::AnthropicModel;
pub use trigger::{RollingTranscript, TriggerConfig};

use audio_core::SourceKind;
use tokio::sync::broadcast;
use transcribe::TranscriptBus;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("ANTHROPIC_API_KEY is not set")]
    MissingApiKey,
    #[error("request to the Anthropic API failed: {0}")]
    Http(String),
    #[error("the Anthropic API returned an error: {0}")]
    Api(String),
    #[error("could not parse the model's response: {0}")]
    MalformedResponse(String),
}

/// What the model proposes (or declines to — `None` is the common case and
/// a *good* outcome: most conversational moments don't deserve a popup).
#[derive(Debug, Clone)]
pub struct ModelSuggestion {
    /// Short verbatim-ish fragment of the transcript that triggered this.
    pub cue: String,
    /// One imperative line — the card's payload.
    pub hint: String,
    /// Deeper guidance revealed on expand. Tier-1 produces it in the same
    /// call for now; a true Tier-2 on-click agent can replace this later
    /// without changing anything upstream.
    pub detail: String,
}

/// The seam between triggering logic and the actual LLM, so tests (and any
/// future local model) can stand in for the API.
#[async_trait::async_trait]
pub trait SuggestionModel: Send + Sync {
    /// `transcript` is the rendered rolling context; `recent_hints` are
    /// the last few suggestions already shown, for the model to avoid
    /// repeating itself.
    async fn suggest(
        &self,
        transcript: &str,
        recent_hints: &[String],
    ) -> Result<Option<ModelSuggestion>, AgentError>;
}

/// A finished suggestion, tagged with the stream whose segment triggered
/// it — the same you-vs-them separation, carried one stage further.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub id: String,
    pub source: SourceKind,
    pub cue: String,
    pub hint: String,
    pub detail: String,
}

/// Broadcast bus for suggestions; identical contract to `AudioBus` and
/// `TranscriptBus`. Suggestions arrive a few per minute at most, so
/// lagging is a non-issue — the uniformity is the point.
#[derive(Clone)]
pub struct SuggestionBus {
    tx: broadcast::Sender<Suggestion>,
}

impl SuggestionBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Suggestion> {
        self.tx.subscribe()
    }

    pub fn publish(&self, suggestion: Suggestion) {
        let _ = self.tx.send(suggestion);
    }
}

/// How many prior hints are shown to the model for self-dedup.
const RECENT_HINT_MEMORY: usize = 4;

/// Consumes `transcript_bus` until it closes, publishing suggestions to
/// `suggestion_bus`. Spawn as a task; abort to stop.
pub async fn run_cue_agent(
    transcript_bus: TranscriptBus,
    suggestion_bus: SuggestionBus,
    model: std::sync::Arc<dyn SuggestionModel>,
    config: TriggerConfig,
) {
    let mut rx = transcript_bus.subscribe();
    let mut context = RollingTranscript::new(config);
    let mut recent_hints: Vec<String> = Vec::new();
    let mut next_id = 0u64;

    loop {
        match rx.recv().await {
            Ok(segment) => {
                context.push(segment.source, &segment.text);
                if !context.should_fire() {
                    continue;
                }
                context.mark_fired();

                // The model call is awaited inline, so segments arriving
                // mid-call queue up in the bus and are folded into context
                // before the *next* call — natural batching, no backlog of
                // stale calls.
                match model.suggest(&context.render(), &recent_hints).await {
                    Ok(Some(proposal)) => {
                        recent_hints.push(proposal.hint.clone());
                        if recent_hints.len() > RECENT_HINT_MEMORY {
                            recent_hints.remove(0);
                        }
                        next_id += 1;
                        suggestion_bus.publish(Suggestion {
                            id: format!("cue-{next_id}"),
                            source: segment.source,
                            cue: proposal.cue,
                            hint: proposal.hint,
                            detail: proposal.detail,
                        });
                    }
                    Ok(None) => {} // nothing worth saying — the common case
                    Err(e) => eprintln!("[cue-agent] model call failed: {e}"),
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
