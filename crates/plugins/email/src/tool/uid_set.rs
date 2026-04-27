//! Helpers for IMAP UID set strings.

/// Format a UID list as an IMAP `sequence-set` (comma-joined). Caller
/// passes a small set (∼≤100); IMAP wire-protocol limits don't bite
/// in practice for the agent's bulk ops.
pub fn format_uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_comma_joined() {
        assert_eq!(format_uid_set(&[1, 2, 3]), "1,2,3");
    }
    #[test]
    fn empty_yields_empty_string() {
        assert_eq!(format_uid_set(&[]), "");
    }
}
