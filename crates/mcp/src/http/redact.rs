//! Redact sensitive fragments from URLs before logging.
//!
//! `reqwest::Url::Display` exposes `https://user:pass@host/path`. Any log
//! that prints that verbatim leaks credentials into structured logs. Use
//! this helper instead.

/// Replace the userinfo component (`user:pass@`) with `***@`. If parsing
/// fails, return the input as-is (we'd rather log something than nothing).
pub fn redact_sensitive_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    if url.username().is_empty() && url.password().is_none() {
        return url.to_string();
    }
    let _ = url.set_username("***");
    let _ = url.set_password(None);
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_userinfo_passthrough() {
        assert_eq!(
            redact_sensitive_url("https://example.com/mcp"),
            "https://example.com/mcp"
        );
    }

    #[test]
    fn user_and_password_redacted() {
        assert_eq!(
            redact_sensitive_url("https://user:pass@example.com/mcp"),
            "https://***@example.com/mcp"
        );
    }

    #[test]
    fn user_only_redacted() {
        assert_eq!(
            redact_sensitive_url("https://user@example.com/"),
            "https://***@example.com/"
        );
    }

    #[test]
    fn malformed_url_returned_as_is() {
        assert_eq!(redact_sensitive_url("not a url"), "not a url");
    }
}
