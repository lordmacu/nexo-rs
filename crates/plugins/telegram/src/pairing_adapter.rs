//! `PairingChannelAdapter` impl for the Telegram plugin.
//!
//! Telegram chat ids are numeric (signed 64-bit) for users / groups
//! and `@username` for public bots. We pass the numeric form through
//! verbatim and lower-case the `@` form so two messages from the same
//! user with different casing land on the same allowlist row.
//!
//! For challenge replies we use Telegram's MarkdownV2 so the pairing
//! code renders as inline code (monospace, easy to long-press copy).
//! MarkdownV2 reserves a long set of punctuation chars; we escape
//! every reserved char in the literal text and the code so the
//! Bot API's strict parser accepts the message.

use async_trait::async_trait;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_pairing::adapter::PairingChannelAdapter;
use serde_json::json;

pub struct TelegramPairingAdapter {
    broker: AnyBroker,
}

impl TelegramPairingAdapter {
    pub fn new(broker: AnyBroker) -> Self {
        Self { broker }
    }

    fn outbound_topic(account: &str) -> String {
        if account.is_empty() {
            "plugin.outbound.telegram".to_string()
        } else {
            format!("plugin.outbound.telegram.{account}")
        }
    }
}

/// Escape every MarkdownV2 reserved character so the Bot API parser
/// doesn't choke. Reserved set per
/// <https://core.telegram.org/bots/api#markdownv2-style>.
fn escape_markdown_v2(s: &str) -> String {
    const RESERVED: &str = "_*[]()~`>#+-=|{}.!\\";
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if RESERVED.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[async_trait]
impl PairingChannelAdapter for TelegramPairingAdapter {
    fn channel_id(&self) -> &'static str {
        "telegram"
    }

    fn normalize_sender(&self, raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix('@') {
            if rest.is_empty() {
                return None;
            }
            Some(format!("@{}", rest.to_ascii_lowercase()))
        } else {
            Some(trimmed.to_string())
        }
    }

    fn format_challenge_text(&self, code: &str) -> String {
        // Wrap the code in backticks so it renders as inline code; the
        // backticks themselves are not escaped (they're MarkdownV2
        // markup), but the literal text and the rest of the line are.
        let escaped_code = escape_markdown_v2(code);
        let line1 = escape_markdown_v2("🔐 Pairing required.");
        let line2 = escape_markdown_v2("Ask the operator to run:");
        format!("{line1}\n{line2}\n`nexo pair approve {escaped_code}`",)
    }

    async fn send_reply(&self, account: &str, to: &str, text: &str) -> anyhow::Result<()> {
        let topic = Self::outbound_topic(account);
        let payload = json!({
            "kind": "text",
            "to": to,
            "text": text,
            "parse_mode": "MarkdownV2",
        });
        let evt = Event::new(&topic, "core.pairing", payload);
        self.broker
            .publish(&topic, evt)
            .await
            .map_err(|e| anyhow::anyhow!("telegram pairing publish failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> TelegramPairingAdapter {
        TelegramPairingAdapter::new(AnyBroker::local())
    }

    #[test]
    fn at_username_is_lowercased() {
        let a = adapter();
        assert_eq!(
            a.normalize_sender("@User_Name").as_deref(),
            Some("@user_name")
        );
    }

    #[test]
    fn numeric_chat_id_passes_through() {
        let a = adapter();
        assert_eq!(
            a.normalize_sender("123456789").as_deref(),
            Some("123456789")
        );
        assert_eq!(
            a.normalize_sender("-1001234567890").as_deref(),
            Some("-1001234567890")
        );
    }

    #[test]
    fn empty_or_bare_at_returns_none() {
        let a = adapter();
        assert!(a.normalize_sender("").is_none());
        assert!(a.normalize_sender("   ").is_none());
        assert!(a.normalize_sender("@").is_none());
    }

    #[test]
    fn escape_markdown_v2_handles_reserved_chars() {
        assert_eq!(escape_markdown_v2("a.b"), "a\\.b");
        assert_eq!(escape_markdown_v2("(x)"), "\\(x\\)");
        assert_eq!(escape_markdown_v2("a-b_c"), "a\\-b\\_c");
        assert_eq!(escape_markdown_v2("plain"), "plain");
        assert_eq!(escape_markdown_v2("!"), "\\!");
    }

    #[test]
    fn format_challenge_text_wraps_code_in_backticks_and_escapes_periods() {
        let a = adapter();
        let s = a.format_challenge_text("ABCD-1234");
        assert!(s.contains("\\."), "literal text periods escaped: {s}");
        assert!(
            s.contains("`nexo pair approve ABCD\\-1234`"),
            "code wrapped + escaped: {s}"
        );
    }

    #[test]
    fn outbound_topic_handles_empty_and_named_account() {
        assert_eq!(
            TelegramPairingAdapter::outbound_topic(""),
            "plugin.outbound.telegram"
        );
        assert_eq!(
            TelegramPairingAdapter::outbound_topic("sales"),
            "plugin.outbound.telegram.sales",
        );
    }

    #[test]
    fn channel_id_is_telegram() {
        let a = adapter();
        assert_eq!(a.channel_id(), "telegram");
    }
}
