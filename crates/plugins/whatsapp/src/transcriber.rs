//! Voice-note → text bridge.
//!
//! `wa-agent`'s `run_agent_with_transcribe` accepts any value that
//! implements [`whatsapp_rs::agent::Transcriber`]. We implement it by
//! round-tripping through the broker — the agent framework already
//! carries Skills (Phase 13.6 whisper, etc.) on topics of the form
//! `skill.<id>.transcribe`, so our job is just to serialise the audio
//! bytes + mimetype, publish, and relay the reply back into `ctx.text`.
//!
//! Config toggles whether this Transcriber is attached (`transcriber.enabled`).
//! When off, the plugin uses plain `run_agent_with` and audio messages
//! surface as media-only events.

use std::time::Duration;

use agent_broker::{AnyBroker, BrokerHandle, Message};
use base64::Engine;
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;

pub struct NatsTranscriber {
    broker: AnyBroker,
    topic: String,
    timeout: Duration,
}

impl NatsTranscriber {
    pub fn new(broker: AnyBroker, skill: &str, timeout: Duration) -> Self {
        Self {
            broker,
            topic: format!("skill.{skill}.transcribe"),
            timeout,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TranscribeReply {
    #[serde(default)]
    text: Option<String>,
}

impl whatsapp_rs::agent::Transcriber for NatsTranscriber {
    fn transcribe(
        &self,
        audio: Vec<u8>,
        mimetype: String,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        let broker = self.broker.clone();
        let topic = self.topic.clone();
        let timeout = self.timeout;
        Box::pin(async move {
            let payload = serde_json::json!({
                "audio_b64": base64::engine::general_purpose::STANDARD.encode(&audio),
                "mime": mimetype,
            });
            let msg = Message::new(&topic, payload);
            match broker.request(&topic, msg, timeout).await {
                Ok(reply) => {
                    match serde_json::from_value::<TranscribeReply>(reply.payload) {
                        Ok(r) => r.text,
                        Err(e) => {
                            tracing::warn!(error = %e, "transcribe reply parse failed");
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, topic = %topic, "transcribe request failed");
                    None
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_broker::AnyBroker;
    use std::time::Duration;

    #[test]
    fn topic_derived_from_skill_id() {
        let t = NatsTranscriber::new(AnyBroker::local(), "whisper", Duration::from_secs(1));
        assert_eq!(t.topic, "skill.whisper.transcribe");
    }
}
