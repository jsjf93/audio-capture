//! The real `SuggestionModel`: Anthropic's Messages API with a small,
//! fast model (Haiku) — Tier 1 runs on every trigger, so per-call cost and
//! latency dominate the choice.
//!
//! Two API features carry the design:
//! - **Tool use as structured output.** The model gets two tools:
//!   `propose_suggestion(cue, hint, detail)` and `update_notes(notes)`,
//!   and may call both, either, or neither. Parsing typed tool calls is
//!   far more reliable than asking for JSON in prose, and "no tool call"
//!   is an unambiguous, well-typed "nothing worth saying".
//! - **Prompt caching.** The system prompt (per mode, see modes.rs) is
//!   marked cacheable; only the notes + rolling transcript change between
//!   calls, so repeat calls within the cache TTL bill the instructions at
//!   the cached rate.

use crate::{AgentError, ModelOutcome, ModelSuggestion, SuggestionModel};
use serde_json::{json, Value};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 700;

pub struct AnthropicModel {
    client: reqwest::Client,
    api_key: String,
    /// The mode's composed system prompt (see `modes::mode_profile`).
    system_prompt: String,
}

impl AnthropicModel {
    /// Reads `ANTHROPIC_API_KEY` from the environment. Dev-time
    /// arrangement — the key moves to the macOS Keychain when settings
    /// exist.
    pub fn from_env(system_prompt: String) -> Result<Self, AgentError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| AgentError::MissingApiKey)?;
        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
            system_prompt,
        })
    }
}

#[async_trait::async_trait]
impl SuggestionModel for AnthropicModel {
    async fn suggest(
        &self,
        transcript: &str,
        notes: &str,
        recent_hints: &[String],
    ) -> Result<ModelOutcome, AgentError> {
        let already_shown = if recent_hints.is_empty() {
            "none yet".to_string()
        } else {
            recent_hints
                .iter()
                .map(|h| format!("- {h}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let notes = if notes.is_empty() { "(none yet)" } else { notes };

        let body = json!({
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "system": [{
                "type": "text",
                "text": self.system_prompt,
                "cache_control": { "type": "ephemeral" }
            }],
            "tools": [
                {
                    "name": "propose_suggestion",
                    "description": "Propose the one suggestion worth showing the user right now. Only call this when the bar described in your instructions is met.",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "value": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Honest 1-10 score of how much showing this right now helps the user" },
                            "cue": { "type": "string", "description": "Short transcript fragment that triggered this" },
                            "hint": { "type": "string", "description": "One imperative sentence, max 12 words" },
                            "detail": { "type": "string", "description": "2-4 sentences of expanded guidance" }
                        },
                        "required": ["value", "cue", "hint", "detail"]
                    }
                },
                {
                    "name": "update_notes",
                    "description": "Replace your running notes about this conversation. Call whenever this exchange taught you something durable; send the complete replacement text.",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "notes": { "type": "string", "description": "Complete replacement notes, max ~150 words, tight and factual" }
                        },
                        "required": ["notes"]
                    }
                }
            ],
            "messages": [{
                "role": "user",
                "content": format!(
                    "Suggestions already shown (do not repeat):\n{already_shown}\n\n\
                     Your running notes from earlier in this conversation:\n{notes}\n\n\
                     Transcript (the recent part of the conversation):\n{transcript}"
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

        // Collect whichever tool calls arrived; plain text (the "pass"
        // path) yields the default empty outcome — by design, not an error.
        let Some(blocks) = payload["content"].as_array() else {
            return Err(AgentError::MalformedResponse(
                "response has no content array".into(),
            ));
        };
        let mut outcome = ModelOutcome::default();
        for block in blocks {
            if block["type"] != "tool_use" {
                continue;
            }
            let input = &block["input"];
            let field = |name: &str| -> Result<String, AgentError> {
                input[name]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| {
                        AgentError::MalformedResponse(format!("tool input missing `{name}`"))
                    })
            };
            match block["name"].as_str() {
                Some("propose_suggestion") => {
                    let value = input["value"].as_u64().ok_or_else(|| {
                        AgentError::MalformedResponse("tool input missing `value`".into())
                    })?;
                    outcome.suggestion = Some(ModelSuggestion {
                        cue: field("cue")?,
                        hint: field("hint")?,
                        detail: field("detail")?,
                        value: value.clamp(1, 10) as u8,
                    });
                }
                Some("update_notes") => {
                    outcome.updated_notes = Some(field("notes")?);
                }
                _ => {}
            }
        }
        Ok(outcome)
    }
}
