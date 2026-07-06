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
mod modes;
mod trigger;

pub use anthropic::AnthropicModel;
pub use modes::{available_modes, mode_profile, ModeProfile};
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

/// The model's best candidate for the current moment, with an honest
/// value score. The model is *not* the gatekeeper: it always proposes its
/// best candidate and scores it; whether the score clears the mode's bar
/// is decided in `run_cue_agent`. (Learned the hard way — asking a small
/// model the binary "is this worth interrupting for?" yields near-constant
/// "no" in real meetings.)
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
    /// 1–10, how much showing this right now would help. Filtered against
    /// `ModeProfile::min_suggestion_value`.
    pub value: u8,
}

/// Everything one model call can produce: possibly a suggestion, possibly
/// an update to the running notes, possibly both or neither.
#[derive(Debug, Clone, Default)]
pub struct ModelOutcome {
    pub suggestion: Option<ModelSuggestion>,
    /// Full replacement text for the running notes — the agent's only
    /// memory beyond the recent transcript window.
    pub updated_notes: Option<String>,
}

/// The seam between triggering logic and the actual LLM, so tests (and any
/// future local model) can stand in for the API.
#[async_trait::async_trait]
pub trait SuggestionModel: Send + Sync {
    /// `transcript` is the rendered recent window, `notes` the running
    /// memory from previous calls, and `recent_hints` the last few
    /// suggestions already shown (for the model to avoid repeats).
    async fn suggest(
        &self,
        transcript: &str,
        notes: &str,
        recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError>;
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

/// How many prior hints are remembered — shown to the model for self-dedup
/// AND enforced app-side by `is_near_duplicate` (the model rewords repeats
/// past its own instruction when the underlying situation persists).
const RECENT_HINT_MEMORY: usize = 8;

/// Content-word set for hint similarity: lowercase, punctuation stripped,
/// crude plural fold ("owners" == "owner"), function words (≤3 chars)
/// dropped so overlap measures content, not grammar.
fn hint_words(text: &str) -> std::collections::HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            let w = w
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            if w.len() > 3 && w.ends_with('s') {
                w[..w.len() - 1].to_string()
            } else {
                w
            }
        })
        .filter(|w| w.len() > 3)
        .collect()
}

/// True when two hints are materially the same suggestion reworded.
/// Threshold is lower than the transcript echo guard's because rephrased
/// suggestions share fewer exact words than two STT passes over the same
/// audio do.
fn is_near_duplicate(a: &str, b: &str) -> bool {
    let (a, b) = (hint_words(a), hint_words(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let intersection = a.intersection(&b).count();
    let union = a.len() + b.len() - intersection;
    intersection as f32 / union as f32 > 0.4
}

/// Consumes `transcript_bus` until it closes, publishing suggestions to
/// `suggestion_bus`. Spawn as a task; abort to stop.
pub async fn run_cue_agent(
    transcript_bus: TranscriptBus,
    suggestion_bus: SuggestionBus,
    model: std::sync::Arc<dyn SuggestionModel>,
    config: TriggerConfig,
    min_suggestion_value: u8,
) {
    let mut rx = transcript_bus.subscribe();
    let mut context = RollingTranscript::new(config);
    let mut notes = String::new();
    let mut recent_hints: Vec<String> = Vec::new();
    let mut next_id = 0u64;

    loop {
        match rx.recv().await {
            Ok(segment) => {
                context.push(segment.source, &segment.text);
                if !context.should_fire() {
                    eprintln!("[cue-agent] segment received, trigger gates not met yet");
                    continue;
                }
                context.mark_fired();
                eprintln!(
                    "[cue-agent] calling model ({} chars of context)",
                    context.render().len()
                );

                // The model call is awaited inline, so segments arriving
                // mid-call queue up in the bus and are folded into context
                // before the *next* call — natural batching, no backlog of
                // stale calls.
                match model.suggest(&context.render(), &notes, &recent_hints).await {
                    Ok(outcome) => {
                        if let Some(updated) = outcome.updated_notes {
                            eprintln!("[cue-agent] notes updated ({} chars)", updated.len());
                            notes = updated;
                        }
                        match outcome.suggestion {
                            Some(proposal)
                                if proposal.value >= min_suggestion_value
                                    && recent_hints
                                        .iter()
                                        .any(|h| is_near_duplicate(h, &proposal.hint)) =>
                            {
                                eprintln!(
                                    "[cue-agent] suppressed repeat suggestion (value {}): {}",
                                    proposal.value, proposal.hint
                                );
                            }
                            Some(proposal) if proposal.value >= min_suggestion_value => {
                                eprintln!(
                                    "[cue-agent] suggestion (value {} ≥ {min_suggestion_value}): {}",
                                    proposal.value, proposal.hint
                                );
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
                            Some(proposal) => {
                                // Calibration signal: what the model *would*
                                // have shown at a lower threshold.
                                eprintln!(
                                    "[cue-agent] candidate below threshold (value {} < {min_suggestion_value}): {}",
                                    proposal.value, proposal.hint
                                );
                            }
                            None => eprintln!("[cue-agent] model passed (nothing actionable at all)"),
                        }
                    }
                    Err(e) => eprintln!("[cue-agent] model call failed: {e}"),
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
