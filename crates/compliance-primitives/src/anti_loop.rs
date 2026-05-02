//! Anti-loop detector.
//!
//! Flags suspicious patterns that indicate a bot is talking to a
//! bot or that an automated reply is bouncing back at us:
//!
//! 1. **Repetition**: same normalised message body seen ≥ N times
//!    in the last `window` duration.
//! 2. **Auto-reply signatures**: phrases that announce
//!    automation (`"Recibido"`, `"Mensaje automático"`, etc.).
//!
//! Microapps `record` every inbound and consult `evaluate` BEFORE
//! dispatching to the LLM. Returning a `LoopVerdict::Loop` from a
//! Phase 83.3 hook short-circuits dispatch with
//! `do_not_reply_again: true`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopVerdict {
    /// Message looks normal — proceed.
    Clear,
    /// Same message body seen `count` times in `window`.
    Repetition { count: usize },
    /// Inbound matches an auto-reply signature.
    AutoReplySignature { phrase: String },
}

#[derive(Debug)]
pub struct AntiLoopDetector {
    threshold: usize,
    window: Duration,
    history: VecDeque<(Instant, String)>,
    signatures: Vec<String>,
}

impl AntiLoopDetector {
    /// Build a detector. `threshold` is the count at which
    /// repetition trips (≥ N occurrences); `window` is the
    /// rolling time window. Default signatures cover Spanish
    /// ("Recibido", "Mensaje automático") and English
    /// ("auto-reply", "out of office") auto-responder phrases.
    pub fn new(threshold: usize, window: Duration) -> Self {
        Self {
            threshold,
            window,
            history: VecDeque::with_capacity(64),
            signatures: vec![
                "recibido".into(),
                "mensaje automatico".into(),
                "mensaje automático".into(),
                "auto-reply".into(),
                "out of office".into(),
                "respuesta automatica".into(),
                "respuesta automática".into(),
            ],
        }
    }

    /// Replace the default signature list with `signatures`. Each
    /// entry is matched case-insensitively against the inbound
    /// body via substring search.
    pub fn with_signatures(mut self, signatures: Vec<String>) -> Self {
        self.signatures = signatures
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        self
    }

    /// Record an inbound and return the verdict in one call.
    /// Cheap O(history). The history is bounded by `window` —
    /// older entries are dropped on every `evaluate`.
    pub fn record_and_evaluate(&mut self, body: &str) -> LoopVerdict {
        self.evaluate_at(body, Instant::now())
    }

    /// Test-only / time-injectable variant of
    /// [`record_and_evaluate`].
    #[doc(hidden)]
    pub fn evaluate_at(&mut self, body: &str, now: Instant) -> LoopVerdict {
        let normalised = normalise(body);

        // Auto-reply signature check first — these short-circuit
        // even if the signature only appears once.
        let lowered = normalised.to_ascii_lowercase();
        for sig in &self.signatures {
            if lowered.contains(sig) {
                return LoopVerdict::AutoReplySignature {
                    phrase: sig.clone(),
                };
            }
        }

        // Trim history older than `window`.
        while let Some((ts, _)) = self.history.front() {
            if now.duration_since(*ts) > self.window {
                self.history.pop_front();
            } else {
                break;
            }
        }

        self.history.push_back((now, normalised.clone()));

        let count = self
            .history
            .iter()
            .filter(|(_, b)| b == &normalised)
            .count();
        if count >= self.threshold {
            LoopVerdict::Repetition { count }
        } else {
            LoopVerdict::Clear
        }
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

/// Normalise for repetition matching: lowercase, collapse
/// whitespace runs, trim. Avoids bouncing the same message with
/// different capitalisation past the threshold.
fn normalise(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut last_was_space = true;
    for c in body.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_detector_clears_first_message() {
        let mut d = AntiLoopDetector::new(3, Duration::from_secs(60));
        let v = d.record_and_evaluate("hola");
        assert_eq!(v, LoopVerdict::Clear);
    }

    #[test]
    fn repetition_at_threshold_trips() {
        let mut d = AntiLoopDetector::new(3, Duration::from_secs(60));
        assert_eq!(d.record_and_evaluate("ping"), LoopVerdict::Clear);
        assert_eq!(d.record_and_evaluate("ping"), LoopVerdict::Clear);
        let v = d.record_and_evaluate("ping");
        assert_eq!(v, LoopVerdict::Repetition { count: 3 });
    }

    #[test]
    fn whitespace_and_case_normalise() {
        let mut d = AntiLoopDetector::new(2, Duration::from_secs(60));
        d.record_and_evaluate("Hola Mundo");
        let v = d.record_and_evaluate("  hola   mundo  ");
        assert_eq!(v, LoopVerdict::Repetition { count: 2 });
    }

    #[test]
    fn entries_outside_window_drop() {
        let mut d = AntiLoopDetector::new(2, Duration::from_millis(100));
        let t0 = Instant::now();
        d.evaluate_at("ping", t0);
        // 200 ms later — first entry should expire.
        let t1 = t0 + Duration::from_millis(200);
        let v = d.evaluate_at("ping", t1);
        assert_eq!(v, LoopVerdict::Clear);
        // Only the latest entry survived.
        assert_eq!(d.history_len(), 1);
    }

    #[test]
    fn auto_reply_signature_short_circuits() {
        let mut d = AntiLoopDetector::new(99, Duration::from_secs(3600));
        let v = d.record_and_evaluate("Recibido. Te contestaré pronto.");
        match v {
            LoopVerdict::AutoReplySignature { phrase } => {
                assert!(phrase.contains("recibido"));
            }
            other => panic!("expected AutoReplySignature, got {other:?}"),
        }
    }

    #[test]
    fn auto_reply_signature_in_english() {
        let mut d = AntiLoopDetector::new(99, Duration::from_secs(3600));
        let v = d.record_and_evaluate("This is an auto-reply.");
        match v {
            LoopVerdict::AutoReplySignature { phrase } => {
                assert!(phrase.contains("auto-reply"));
            }
            other => panic!("expected auto-reply, got {other:?}"),
        }
    }

    #[test]
    fn custom_signatures_replace_defaults() {
        let mut d = AntiLoopDetector::new(99, Duration::from_secs(3600))
            .with_signatures(vec!["VACATION".into()]);
        // The default "auto-reply" signature is gone.
        assert_eq!(
            d.record_and_evaluate("This is an auto-reply"),
            LoopVerdict::Clear
        );
        // The custom one fires.
        match d.record_and_evaluate("On vacation, will reply soon") {
            LoopVerdict::AutoReplySignature { phrase } => {
                assert_eq!(phrase, "vacation");
            }
            other => panic!("expected match, got {other:?}"),
        }
    }

    #[test]
    fn distinct_messages_do_not_count_toward_threshold() {
        let mut d = AntiLoopDetector::new(2, Duration::from_secs(60));
        d.record_and_evaluate("hola");
        d.record_and_evaluate("mundo");
        let v = d.record_and_evaluate("hola");
        // Only 2 instances of "hola" — at threshold.
        assert_eq!(v, LoopVerdict::Repetition { count: 2 });
    }
}
