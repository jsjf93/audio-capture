//! The real `SuggestionModel`: Anthropic's Messages API with a small,
//! fast model (Haiku) — Tier 1 runs on every trigger, so per-call cost and
//! latency dominate the choice.
//!
//! Two API features carry the design:
//! - **Tool use as structured output.** The model is given exactly one
//!   tool, `propose_suggestion(cue, hint, detail)`, and told that *not*
//!   calling it is the normal outcome. Parsing a typed tool call is far
//!   more reliable than asking for JSON in prose, and "no tool call"
//!   is an unambiguous, well-typed "nothing worth saying".
//! - **Prompt caching.** The system prompt is marked cacheable; only the
//!   rolling transcript changes between calls, so repeat calls within the
//!   cache TTL bill the instructions at the cached rate.

use crate::{AgentError, ModelSuggestion, SuggestionModel};
use serde_json::{json, Value};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 400;

const SYSTEM_PROMPT: &str = "\
You are a silent assistant watching a live work conversation (a sales call, \
interview, or meeting) through a rolling transcript. Lines tagged [you] are \
the user you are helping; lines tagged [them] are everyone else. The \
transcript comes from live speech recognition, so expect missing \
punctuation and occasional mis-heard words.

Each time you are shown the transcript, decide whether there is ONE \
suggestion valuable enough to interrupt the user's glance for. The bar is \
high: an unasked question that matters, a buying signal or objection worth \
acting on now, something the user deflected that will come back, a concrete \
next step going unanchored. If the moment is ordinary — greetings, \
logistics, the user already doing the right thing — do NOT call the tool; \
reply with the single word: pass.

When you do suggest, call propose_suggestion exactly once:
- cue: a short fragment quoted or near-quoted from the transcript that \
triggered you (a few words).
- hint: one imperative sentence, at most 12 words. It must be readable in \
one second.
- detail: 2-4 sentences of deeper guidance for if the user clicks to \
expand: why this matters now and how to act on it.

Never repeat or rephrase a suggestion listed as already shown.";

pub struct AnthropicModel {
    client: reqwest::Client,
    api_key: String,
}

impl AnthropicModel {
    /// Reads `ANTHROPIC_API_KEY` from the environment. Dev-time
    /// arrangement — the key moves to the macOS Keychain when settings
    /// exist.
    pub fn from_env() -> Result<Self, AgentError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| AgentError::MissingApiKey)?;
        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
        })
    }
}

#[async_trait::async_trait]
impl SuggestionModel for AnthropicModel {
    async fn suggest(
        &self,
        transcript: &str,
        recent_hints: &[String],
    ) -> Result<Option<ModelSuggestion>, AgentError> {
        let already_shown = if recent_hints.is_empty() {
            "none yet".to_string()
        } else {
            recent_hints
                .iter()
                .map(|h| format!("- {h}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let body = json!({
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "system": [{
                "type": "text",
                "text": SYSTEM_PROMPT,
                "cache_control": { "type": "ephemeral" }
            }],
            "tools": [{
                "name": "propose_suggestion",
                "description": "Propose the one suggestion worth showing the user right now. Only call this when the bar described in your instructions is met.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "cue": { "type": "string", "description": "Short transcript fragment that triggered this" },
                        "hint": { "type": "string", "description": "One imperative sentence, max 12 words" },
                        "detail": { "type": "string", "description": "2-4 sentences of expanded guidance" }
                    },
                    "required": ["cue", "hint", "detail"]
                }
            }],
            "messages": [{
                "role": "user",
                "content": format!(
                    "Suggestions already shown (do not repeat):\n{already_shown}\n\nTranscript so far:\n{transcript}"
                )
            }]
        });

        let response = self
            .client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Http(e.to_string()))?;

        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|e| AgentError::MalformedResponse(e.to_string()))?;

        if !status.is_success() {
            let message = payload["error"]["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string();
            return Err(AgentError::Api(format!("{status}: {message}")));
        }

        // Find the tool call, if any. Plain text (the "pass" path) means
        // no suggestion — by design, not an error.
        let Some(blocks) = payload["content"].as_array() else {
            return Err(AgentError::MalformedResponse(
                "response has no content array".into(),
            ));
        };
        for block in blocks {
            if block["type"] == "tool_use" && block["name"] == "propose_suggestion" {
                let input = &block["input"];
                let field = |name: &str| -> Result<String, AgentError> {
                    input[name]
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| AgentError::MalformedResponse(format!("tool input missing `{name}`")))
                };
                return Ok(Some(ModelSuggestion {
                    cue: field("cue")?,
                    hint: field("hint")?,
                    detail: field("detail")?,
                }));
            }
        }
        Ok(None)
    }
}
