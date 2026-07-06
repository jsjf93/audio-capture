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
        // Mirrors the "general" mode profile; modes.rs is where per-mode
        // tuning actually lives.
        Self {
            cooldown: Duration::from_secs(8),
            min_new_words: 6,
            context_char_budget: 12_000,
        }
    }
}

/// How far back to look when checking whether a new segment is a speaker
/// echo of the other stream (see `push`).
const ECHO_WINDOW: Duration = Duration::from_secs(10);

struct Entry {
    source: SourceKind,
    text: String,
    at: Instant,
    words: std::collections::HashSet<String>,
}

fn normalized_words(text: &str) -> std::collections::HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-set Jaccard similarity — crude but robust against the small
/// wording differences two STT passes produce over the same audio
/// ("it's Micah over at" vs "it's uh, Micah, um, over at").
fn is_echo_of(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    intersection as f32 / union as f32 > 0.55
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
        let words = normalized_words(text);

        // Speaker-echo guard, discovered necessary in real-world testing:
        // when call audio plays through a speaker the mic can hear (any
        // input/output device mismatch defeats hardware echo cancellation),
        // the same sentence arrives on *both* streams a moment apart, and
        // the doubled transcript reads as two people talking over each
        // other — which reliably confuses the model into passing. A
        // near-duplicate of a recent segment from the *other* stream is
        // dropped; the first arrival keeps the attribution.
        let now = Instant::now();
        let is_echo = self
            .entries
            .iter()
            .rev()
            .take_while(|e| now.duration_since(e.at) < ECHO_WINDOW)
            .any(|e| e.source != source && is_echo_of(&words, &e.words));
        if is_echo {
            return;
        }

        self.words_since_fire += text.split_whitespace().count();
        self.total_chars += text.len();
        self.entries.push_back(Entry {
            source,
            text: text.to_string(),
            at: now,
            words,
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
        t.push(SourceKind::Microphone, "ok");
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
    fn near_duplicate_from_the_other_stream_is_dropped_as_echo() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "yeah hey Edward it's Micah over at NOPL");
        // The mic hears the speaker a moment later, slightly differently.
        t.push(SourceKind::Microphone, "yeah Edward it's uh Micah um over at NOPL");
        let rendered = t.render();
        assert_eq!(
            rendered.matches("Micah").count(),
            1,
            "echoed line should appear once, got:\n{rendered}"
        );
        assert!(rendered.contains("[them]"), "first arrival keeps attribution");
    }

    #[test]
    fn echoes_do_not_count_toward_the_word_gate() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "could you send me a quick email");
        t.mark_fired();
        std::thread::sleep(Duration::from_millis(60));
        t.push(SourceKind::Microphone, "could you send me a quick email");
        assert!(
            !t.should_fire(),
            "an echo alone must not re-trigger a model call"
        );
    }

    #[test]
    fn same_words_from_the_same_stream_are_kept() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "no no no wait a moment");
        t.push(SourceKind::SystemOutput, "no no no wait a moment");
        assert_eq!(t.render().matches("wait a moment").count(), 2);
    }

    #[test]
    fn different_content_from_the_other_stream_is_kept() {
        let mut t = RollingTranscript::new(config());
        t.push(SourceKind::SystemOutput, "could you send me a quick email instead");
        t.push(SourceKind::Microphone, "sure no problem I will do that right away");
        let rendered = t.render();
        assert!(rendered.contains("[them]") && rendered.contains("[you]"));
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
