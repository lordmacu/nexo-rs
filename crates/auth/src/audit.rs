//! Audit log helpers. Every outbound publish that crosses a channel
//! boundary should emit through [`audit_outbound`] so the
//! `credentials.audit` target is a single append-only trail.

use crate::handle::CredentialHandle;

pub const TARGET: &str = "credentials.audit";

pub fn audit_outbound(handle: &CredentialHandle, topic: &str) {
    tracing::info!(
        target: TARGET,
        agent = handle.agent_id(),
        channel = handle.channel(),
        fp = %handle.fingerprint(),
        topic = topic,
        direction = "outbound",
    );
}

pub fn audit_inbound(handle: &CredentialHandle, topic: &str) {
    tracing::info!(
        target: TARGET,
        agent = handle.agent_id(),
        channel = handle.channel(),
        fp = %handle.fingerprint(),
        topic = topic,
        direction = "inbound",
    );
}

pub fn audit_denied(channel: &'static str, instance: &str, agent: &str, reason: &str) {
    tracing::warn!(
        target: TARGET,
        channel = channel,
        instance = instance,
        agent = agent,
        reason = reason,
        event = "denied",
    );
}
