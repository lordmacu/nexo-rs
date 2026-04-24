/// Matches a NATS-style topic pattern against a subject.
///
/// Rules:
/// - `*` matches exactly one segment
/// - `>` matches one or more remaining segments (only valid as the last token)
/// - Literal tokens must match exactly
pub fn topic_matches(pattern: &str, subject: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('.').collect();
    let sub_parts: Vec<&str> = subject.split('.').collect();

    match_parts(&pat_parts, &sub_parts)
}

fn match_parts(pat: &[&str], sub: &[&str]) -> bool {
    match (pat.first(), sub.first()) {
        (None, None) => true,
        (Some(&">"), _) => !sub.is_empty(),
        (Some(&"*"), Some(_)) => match_parts(&pat[1..], &sub[1..]),
        (Some(p), Some(s)) if *p == *s => match_parts(&pat[1..], &sub[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::topic_matches;

    #[test]
    fn exact_match() {
        assert!(topic_matches("agent.events.kate", "agent.events.kate"));
    }

    #[test]
    fn exact_no_match() {
        assert!(!topic_matches("agent.events.kate", "agent.events.ventas"));
    }

    #[test]
    fn wildcard_single_segment() {
        assert!(topic_matches("agent.events.*", "agent.events.kate"));
        assert!(topic_matches("agent.events.*", "agent.events.ventas"));
    }

    #[test]
    fn wildcard_single_does_not_match_multiple_segments() {
        assert!(!topic_matches("agent.events.*", "agent.events.kate.click"));
    }

    #[test]
    fn wildcard_rest_matches_multiple() {
        assert!(topic_matches("agent.>", "agent.events.kate"));
        assert!(topic_matches("agent.>", "agent.events.kate.click"));
    }

    #[test]
    fn wildcard_rest_does_not_cross_prefix() {
        assert!(!topic_matches("agent.>", "plugin.inbound.wa"));
    }

    #[test]
    fn wildcard_rest_requires_at_least_one_segment() {
        assert!(!topic_matches("agent.>", "agent"));
    }

    #[test]
    fn wildcard_rest_at_root() {
        assert!(topic_matches(">", "agent.events.kate"));
        assert!(topic_matches(">", "plugin.inbound.wa"));
    }

    #[test]
    fn mixed_wildcards() {
        assert!(topic_matches("agent.*.kate", "agent.events.kate"));
        assert!(!topic_matches("agent.*.kate", "agent.events.ventas"));
    }
}
