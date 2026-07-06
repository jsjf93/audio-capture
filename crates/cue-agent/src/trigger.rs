//! When to spend a model call.
//!
//! Every transcript segment is a *candidate* trigger (segments already end
//! on VAD pauses, so "a pause happened" is implicit), but calling the API
//! on every segment would be wasteful and would flood the user. Two gates:
//! a cooldown since the last call, and a minimum amount of *new* speech
//! since then — a "yeah" arriving after the cooldown shouldn't fire on its
//! own.

use audio_core::SourceKind;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TriggerConfig {
    /// Minimum time between model calls.
    pub cooldown: Duration,
    /// Minimum words accumulated since the last call before firing again.
    pub min_new_words: usize,
    /// Rolling context budget, in characters (~4 chars/token; the model
    /// doesn't need the whole meeting, just the recent shape of it).
    pub context_char_budget: usize,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(8),
            min_new_words: 6,
            context_char_budget: 4_000,
        }
    }
}

struct Entry {
    source: SourceKind,
    text: String,
}

/// The rolling conversation context plus the trigger bookkeeping.
pub struct RollingTranscript {
    config: TriggerConfig,
    entries: VecDeque<Entry>,
    total_chars: usize,
    last_fired_at: Option<Instant>,
    words_since_fire: usize,
}

impl RollingTranscript {
    pub fn new(config: TriggerConfig) -> Self {
        Self {
            config,
            entries: VecDeque::new(),
            total_chars: 0,
            last_fired_at: None,
            words_since_fire: 0,
        }
    }

    pub fn push(&mut self, source: SourceKind, text: &str) {
        self.words_since_fire += text.split_whitespace().count();
        self.total_chars += text.len();
        self.entries.push_back(Entry {
            source,
            text: text.to_string(),
        });
        while self.total_chars > self.config.context_char_budget && self.entries.len() > 1 {
            if let Some(dropped) = self.entries.pop_front() {
                self.total_chars -= dropped.text.len();
            }
        }
    }

    pub fn should_fire(&self) -> bool {
        if self.words_since_fire < self.config.min_new_words {
            return false;
        }
        match self.last_fired_at {
            Some(at) => at.elapsed() >= self.config.cooldown,
            None => true,
        }
    }

    pub fn mark_fired(&mut self) {
        self.last_fired_at = Some(Instant::now());
        self.words_since_fire = 0;
    }

    /// Renders the context the way the system prompt describes it: one
    /// line per segment, tagged [you]/[them].
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.total_chars + self.entries.len() * 8);
        for entry in &self.entries {
            let tag = match entry.source {
                SourceKind::Microphone => "[you]",
                SourceKind::SystemOutput => "[them]",
            };
            out.push_str(tag);
            out.push(' ');
            out.push_str(&entry.text);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TriggerConfig {
        TriggerConfig {
            cooldown: Duration::from_millis(50),
            min_new_words: 5,
            context_char_budget: 200,
        }
    }

    #[test]
    fn fires_only_after_enough_new_words() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "yeah");
        assert!(!t.should_fire(), "one word is not enough");
        t.push(SourceKind::SystemOutput, "we need budget approval before starting");
        assert!(t.should_fire());
    }

    #[test]
    fn cooldown_blocks_immediate_refire() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "we need budget approval before we can start");
        assert!(t.should_fire());
        t.mark_fired();
        t.push(SourceKind::SystemOutput, "and the timeline is quite tight for this year");
        assert!(!t.should_fire(), "cooldown hasn't elapsed");
        std::thread::sleep(Duration::from_millis(60));
        assert!(t.should_fire(), "cooldown elapsed and enough new words");
    }

    #[test]
    fn word_gate_applies_even_after_cooldown() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "we need budget approval before we can start");
        t.mark_fired();
        std::thread::sleep(Duration::from_millis(60));
        t.push(SourceKind::MicrophoneSource_PLACEHOLDER, "ok");
        assert!(!t.should_fire(), "two words since firing is not enough");
    }

    #[test]
    fn context_is_trimmed_to_budget_from_the_front() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::Microphone, &"a".repeat(150));
        t.push(SourceKind::SystemOutput, &"b".repeat(150));
        let rendered = t.render();
        assert!(!rendered.contains('a'), "oldest entry should have been dropped");
        assert!(rendered.contains('b'));
    }

    #[test]
    fn render_tags_speakers() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::Microphone, "hello there");
        t.push(SourceKind::SystemOutput, "hi, thanks for joining");
        let rendered = t.render();
        assert!(rendered.contains("[you] hello there"));
        assert!(rendered.contains("[them] hi, thanks for joining"));
    }
}
