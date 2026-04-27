//! IMAP helper for tool handlers (Phase 48.7).

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use nexo_auth::email::EmailCredentialStore;
use nexo_auth::google::GoogleCredentialStore;
use nexo_config::types::plugins::EmailAccountConfig;

use crate::imap_conn::{ImapConnection, MailboxState};

/// Open a fresh IMAP connection for `account_cfg`, run `f` against a
/// session that has already executed `SELECT <folder>`, and tear
/// down (LOGOUT) afterwards.
pub async fn run_imap_op<F, Fut, T>(
    account_cfg: &EmailAccountConfig,
    creds: &EmailCredentialStore,
    google: Arc<GoogleCredentialStore>,
    folder: &str,
    f: F,
) -> Result<T>
where
    F: FnOnce(ImapConnection, MailboxState) -> Fut,
    Fut: std::future::Future<Output = Result<(ImapConnection, T)>>,
{
    let acct = creds
        .account(&account_cfg.instance)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "no credentials for email instance '{}'",
                account_cfg.instance
            )
        })?;

    let mut conn = ImapConnection::connect(&account_cfg.imap, &acct, google)
        .await
        .with_context(|| {
            format!(
                "email/imap: connect for tool op (instance={})",
                account_cfg.instance
            )
        })?;

    let mb = conn
        .select(folder)
        .await
        .with_context(|| format!("email/imap: SELECT {folder}"))?;

    let (conn, value) = f(conn, mb).await?;
    let _ = conn.logout().await;
    Ok(value)
}

/// Quote a string for use as an IMAP SEARCH atom value (RFC 3501).
pub fn imap_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\r' | '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a `chrono::NaiveDate` as IMAP SEARCH date format
/// (`d-MMM-yyyy`, e.g. `1-Jan-2024`).
pub fn imap_date(date: chrono::NaiveDate) -> String {
    date.format("%-d-%b-%Y").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_backslash_and_quote() {
        assert_eq!(imap_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn quote_collapses_newlines() {
        let q = imap_quote("hi\r\nBcc: evil");
        assert!(!q.contains('\r'));
        assert!(!q.contains('\n'));
        assert!(q.starts_with('"') && q.ends_with('"'));
    }

    #[test]
    fn quote_wraps_simple() {
        assert_eq!(imap_quote("alice@x"), r#""alice@x""#);
    }

    #[test]
    fn date_renders_imap_format() {
        let d = chrono::NaiveDate::from_ymd_opt(2024, 1, 5).unwrap();
        assert_eq!(imap_date(d), "5-Jan-2024");
    }
}
