//! Opt-out matcher.
//!
//! Detects opt-out / unsubscribe keywords in inbound text. Used
//! by Phase 83.3 hooks to vote `Block` + `do_not_reply_again:
//! true` when a user signals they want to be removed from
//! automated outreach.
//!
//! Default phrase list covers Spanish + English. The matcher uses
//! whole-word matching (regex word boundaries) to avoid false
//! positives like `"basta"` matching inside `"basta...".

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptOutVerdict {
    /// Message does not contain an opt-out signal.
    Clear,
    /// Message contains an opt-out keyword.
    OptOut {
        /// The matched keyword (canonical lowercase).
        keyword: String,
    },
}

pub const DEFAULT_PHRASES: &[&str] = &[
    // Spanish
    "no me escribas mas",
    "no me escribas más",
    "no me llames mas",
    "no me llames más",
    "no quiero recibir",
    "darse de baja",
    "darme de baja",
    "baja",
    "cancelar suscripcion",
    "cancelar suscripción",
    "para de enviarme",
    "no contactarme",
    "no me contactes",
    // English
    "stop messaging me",
    "stop texting me",
    "unsubscribe",
    "remove me from",
    "do not contact me",
    "stop sending",
    "leave me alone",
    "opt out",
    "opt-out",
];

#[derive(Debug, Clone)]
pub struct OptOutMatcher {
    /// Pre-compiled per-phrase regex with word boundaries on
    /// each side.
    patterns: Vec<(String, Regex)>,
}

impl Default for OptOutMatcher {
    fn default() -> Self {
        Self::with_default_phrases()
    }
}

impl OptOutMatcher {
    /// Build a matcher seeded with [`DEFAULT_PHRASES`].
    pub fn with_default_phrases() -> Self {
        Self::with_phrases(DEFAULT_PHRASES.iter().map(|s| s.to_string()).collect())
    }

    /// Replace the phrase list. Phrases are lowercased and
    /// wrapped in word-boundary regexes for whole-word
    /// matching. Phrases that fail to compile (regex
    /// metachars in the input) silently fall back to substring
    /// matching — robust against caller mistakes.
    pub fn with_phrases(phrases: Vec<String>) -> Self {
        let patterns = phrases
            .into_iter()
            .map(|p| {
                let lowered = p.to_ascii_lowercase();
                let escaped = regex::escape(&lowered);
                // Word boundary at each end. Allow the keyword
                // to be at start/end of string too. Phrases with
                // spaces still match — the inner spaces are
                // literal.
                let re = Regex::new(&format!(r"(?i)(?:^|\W){}(?:\W|$)", escaped))
                    .unwrap_or_else(|_| Regex::new(&escaped).unwrap());
                (lowered, re)
            })
            .collect();
        Self { patterns }
    }

    /// Append `extras` on top of the existing list (defaults +
    /// extras together).
    pub fn with_extra_phrases(mut self, extras: Vec<String>) -> Self {
        for e in extras {
            let lowered = e.to_ascii_lowercase();
            let escaped = regex::escape(&lowered);
            if let Ok(re) =
                Regex::new(&format!(r"(?i)(?:^|\W){}(?:\W|$)", escaped))
            {
                self.patterns.push((lowered, re));
            }
        }
        self
    }

    /// Evaluate `body`. Returns the first match.
    pub fn evaluate(&self, body: &str) -> OptOutVerdict {
        for (phrase, re) in &self.patterns {
            if re.is_match(body) {
                return OptOutVerdict::OptOut {
                    keyword: phrase.clone(),
                };
            }
        }
        OptOutVerdict::Clear
    }

    pub fn phrase_count(&self) -> usize {
        self.patterns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_clears() {
        let m = OptOutMatcher::default();
        assert_eq!(
            m.evaluate("hola, cuándo abren mañana?"),
            OptOutVerdict::Clear
        );
    }

    #[test]
    fn unsubscribe_matches() {
        let m = OptOutMatcher::default();
        match m.evaluate("please unsubscribe me from this list") {
            OptOutVerdict::OptOut { keyword } => {
                assert_eq!(keyword, "unsubscribe");
            }
            other => panic!("expected match, got {other:?}"),
        }
    }

    #[test]
    fn spanish_no_me_escribas_mas_matches() {
        let m = OptOutMatcher::default();
        let v = m.evaluate("Por favor no me escribas más");
        match v {
            OptOutVerdict::OptOut { keyword } => {
                assert!(keyword.contains("no me escribas"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn opt_out_dash_form_matches() {
        let m = OptOutMatcher::default();
        assert!(matches!(
            m.evaluate("I'd like to opt-out of these messages"),
            OptOutVerdict::OptOut { .. }
        ));
    }

    #[test]
    fn whole_word_matching_avoids_substring_false_positive() {
        // "baja" is in the default list (Spanish "darme de baja"
        // + standalone "baja"). The whole-word regex must not
        // match "abajaron" or similar inflected forms.
        let m = OptOutMatcher::default();
        let v = m.evaluate("los precios bajaron mucho ayer");
        // "baja" is NOT a whole word in "bajaron" — should clear.
        assert_eq!(v, OptOutVerdict::Clear);
    }

    #[test]
    fn case_insensitive() {
        let m = OptOutMatcher::default();
        assert!(matches!(
            m.evaluate("STOP MESSAGING ME"),
            OptOutVerdict::OptOut { .. }
        ));
    }

    #[test]
    fn empty_body_clears() {
        let m = OptOutMatcher::default();
        assert_eq!(m.evaluate(""), OptOutVerdict::Clear);
    }
}
