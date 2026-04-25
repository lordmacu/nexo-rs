//! `PairingChannelAdapter` impl for the WhatsApp plugin.
//!
//! Normalises the JID-style sender ids the WA bridge emits
//! (`<digits>@c.us`, `<digits>@s.whatsapp.net`) into the E.164 form
//! (`+<digits>`) used by the rest of the system, and publishes the
//! pairing reply on the plugin's outbound topic in the same shape the
//! `outbound dispatcher` consumes for proactive text sends.

use async_trait::async_trait;
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_pairing::adapter::PairingChannelAdapter;
use serde_json::json;

pub struct WhatsappPairingAdapter {
    broker: AnyBroker,
}

impl WhatsappPairingAdapter {
    pub fn new(broker: AnyBroker) -> Self {
        Self { broker }
    }

    fn outbound_topic(account: &str) -> String {
        if account.is_empty() {
            "plugin.outbound.whatsapp".to_string()
        } else {
            format!("plugin.outbound.whatsapp.{account}")
        }
    }
}

#[async_trait]
impl PairingChannelAdapter for WhatsappPairingAdapter {
    fn channel_id(&self) -> &'static str {
        "whatsapp"
    }

    fn normalize_sender(&self, raw: &str) -> Option<String> {
        // Strip the `@c.us` / `@s.whatsapp.net` suffix and prepend `+`
        // if the digits don't already carry one. Empty digits → reject.
        let trimmed = raw.trim();
        let stripped = trimmed
            .strip_suffix("@c.us")
            .or_else(|| trimmed.strip_suffix("@s.whatsapp.net"))
            .unwrap_or(trimmed);
        if stripped.is_empty() {
            return None;
        }
        if stripped.starts_with('+') {
            Some(stripped.to_string())
        } else {
            Some(format!("+{stripped}"))
        }
    }

    async fn send_reply(&self, account: &str, to: &str, text: &str) -> anyhow::Result<()> {
        let topic = Self::outbound_topic(account);
        let payload = json!({
            "kind": "text",
            "to": to,
            "text": text,
        });
        let evt = Event::new(&topic, "core.pairing", payload);
        self.broker
            .publish(&topic, evt)
            .await
            .map_err(|e| anyhow::anyhow!("whatsapp pairing publish failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> WhatsappPairingAdapter {
        // Use the local broker — never actually publishes here, the
        // normalize tests don't need it.
        WhatsappPairingAdapter::new(AnyBroker::local())
    }

    #[test]
    fn strips_c_us_suffix_and_adds_plus() {
        let a = adapter();
        assert_eq!(
            a.normalize_sender("573001112222@c.us").as_deref(),
            Some("+573001112222")
        );
    }

    #[test]
    fn strips_s_whatsapp_net_suffix() {
        let a = adapter();
        assert_eq!(
            a.normalize_sender("573001112222@s.whatsapp.net").as_deref(),
            Some("+573001112222"),
        );
    }

    #[test]
    fn keeps_existing_plus_prefix() {
        let a = adapter();
        assert_eq!(
            a.normalize_sender("+573001112222@c.us").as_deref(),
            Some("+573001112222")
        );
    }

    #[test]
    fn no_suffix_still_normalises() {
        let a = adapter();
        assert_eq!(
            a.normalize_sender("573001112222").as_deref(),
            Some("+573001112222")
        );
    }

    #[test]
    fn empty_after_strip_returns_none() {
        let a = adapter();
        assert!(a.normalize_sender("@c.us").is_none());
        assert!(a.normalize_sender("").is_none());
    }

    #[test]
    fn outbound_topic_handles_empty_and_named_account() {
        assert_eq!(
            WhatsappPairingAdapter::outbound_topic(""),
            "plugin.outbound.whatsapp"
        );
        assert_eq!(
            WhatsappPairingAdapter::outbound_topic("primary"),
            "plugin.outbound.whatsapp.primary",
        );
    }

    #[test]
    fn channel_id_is_whatsapp() {
        let a = adapter();
        assert_eq!(a.channel_id(), "whatsapp");
    }
}
