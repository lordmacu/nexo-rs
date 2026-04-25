//! Prometheus metrics for the credential layer. Kept self-contained
//! in this crate so the wiring (main.rs) only has to concatenate
//! [`render_prometheus`] output with the existing `nexo-core`
//! telemetry body.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

use crate::handle::Channel;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ChannelInstance {
    channel: &'static str,
    instance: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct UsageKey {
    channel: &'static str,
    instance: String,
    agent: String,
    direction: &'static str, // "inbound" | "outbound"
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct AclKey {
    channel: &'static str,
    instance: String,
    agent: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ResolveErrorKey {
    channel: &'static str,
    reason: &'static str, // "unbound" | "not_found" | "not_permitted"
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct BindingKey {
    channel: &'static str,
    agent: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct BootErrorKey {
    kind: &'static str,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct RefreshKey {
    account_fp: String,
    outcome: &'static str, // "ok" | "err"
}

static ACCOUNTS_TOTAL: LazyLock<DashMap<&'static str, AtomicU64>> = LazyLock::new(DashMap::new);
static BINDINGS: LazyLock<DashMap<BindingKey, AtomicU64>> = LazyLock::new(DashMap::new);
static USAGE: LazyLock<DashMap<UsageKey, AtomicU64>> = LazyLock::new(DashMap::new);
static ACL_DENIED: LazyLock<DashMap<AclKey, AtomicU64>> = LazyLock::new(DashMap::new);
static RESOLVE_ERRORS: LazyLock<DashMap<ResolveErrorKey, AtomicU64>> = LazyLock::new(DashMap::new);
static BREAKER_STATE: LazyLock<DashMap<ChannelInstance, AtomicI64>> = LazyLock::new(DashMap::new);
static BOOT_ERRORS: LazyLock<DashMap<BootErrorKey, AtomicU64>> = LazyLock::new(DashMap::new);
static INSECURE_PATHS: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static REFRESHES: LazyLock<DashMap<RefreshKey, AtomicU64>> = LazyLock::new(DashMap::new);

pub fn set_accounts_total(channel: Channel, n: u64) {
    ACCOUNTS_TOTAL
        .entry(channel)
        .or_insert_with(|| AtomicU64::new(0))
        .store(n, Ordering::Relaxed);
}

pub fn set_binding(channel: Channel, agent: &str, present: bool) {
    let key = BindingKey {
        channel,
        agent: agent.to_string(),
    };
    BINDINGS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .store(if present { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn inc_usage(channel: Channel, instance: &str, agent: &str, direction: &'static str) {
    let key = UsageKey {
        channel,
        instance: instance.to_string(),
        agent: agent.to_string(),
        direction,
    };
    USAGE
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_acl_denied(channel: Channel, instance: &str, agent: &str) {
    let key = AclKey {
        channel,
        instance: instance.to_string(),
        agent: agent.to_string(),
    };
    ACL_DENIED
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_resolve_error(channel: Channel, reason: &'static str) {
    let key = ResolveErrorKey { channel, reason };
    RESOLVE_ERRORS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn set_breaker_state(channel: Channel, instance: &str, state: BreakerState) {
    let key = ChannelInstance {
        channel,
        instance: instance.to_string(),
    };
    BREAKER_STATE
        .entry(key)
        .or_insert_with(|| AtomicI64::new(0))
        .store(state as i64, Ordering::Relaxed);
}

pub fn inc_boot_error(kind: &'static str) {
    BOOT_ERRORS
        .entry(BootErrorKey { kind })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn set_insecure_paths(n: u64) {
    INSECURE_PATHS.store(n, Ordering::Relaxed);
}

pub fn inc_refresh(account_fp: &str, outcome: &'static str) {
    let key = RefreshKey {
        account_fp: account_fp.to_string(),
        outcome,
    };
    REFRESHES
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

#[derive(Copy, Clone, Debug)]
pub enum BreakerState {
    Closed = 0,
    HalfOpen = 1,
    Open = 2,
}

/// Escape string for use as a Prometheus label value.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the credential subsystem's metrics. Caller concatenates with
/// the rest of the `/metrics` body. Guarantees deterministic series
/// ordering so prompt caching / diff testing works.
pub fn render_prometheus() -> String {
    let mut out = String::new();

    out.push_str("# HELP credentials_accounts_total Credential accounts known per channel.\n");
    out.push_str("# TYPE credentials_accounts_total gauge\n");
    {
        let mut rows: Vec<_> = ACCOUNTS_TOTAL
            .iter()
            .map(|e| (*e.key(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        if rows.is_empty() {
            out.push_str("credentials_accounts_total{channel=\"\"} 0\n");
        }
        for (channel, v) in rows {
            out.push_str(&format!(
                "credentials_accounts_total{{channel=\"{channel}\"}} {v}\n"
            ));
        }
    }

    out.push_str("# HELP credentials_bindings_total 1 when an agent has a binding for the channel, 0 otherwise.\n");
    out.push_str("# TYPE credentials_bindings_total gauge\n");
    {
        let mut rows: Vec<_> = BINDINGS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| (a.0.channel, &a.0.agent).cmp(&(b.0.channel, &b.0.agent)));
        if rows.is_empty() {
            out.push_str("credentials_bindings_total{channel=\"\",agent=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "credentials_bindings_total{{agent=\"{}\",channel=\"{}\"}} {v}\n",
                escape(&key.agent),
                key.channel
            ));
        }
    }

    out.push_str("# HELP channel_account_usage_total Total credential uses by agent/channel/instance/direction.\n");
    out.push_str("# TYPE channel_account_usage_total counter\n");
    {
        let mut rows: Vec<_> = USAGE
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.channel, &a.0.instance, &a.0.agent, a.0.direction).cmp(&(
                b.0.channel,
                &b.0.instance,
                &b.0.agent,
                b.0.direction,
            ))
        });
        if rows.is_empty() {
            out.push_str("channel_account_usage_total{agent=\"\",channel=\"\",direction=\"\",instance=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "channel_account_usage_total{{agent=\"{}\",channel=\"{}\",direction=\"{}\",instance=\"{}\"}} {v}\n",
                escape(&key.agent),
                key.channel,
                key.direction,
                escape(&key.instance),
            ));
        }
    }

    out.push_str("# HELP channel_acl_denied_total Outbound calls rejected by the channel's allow_agents list.\n");
    out.push_str("# TYPE channel_acl_denied_total counter\n");
    {
        let mut rows: Vec<_> = ACL_DENIED
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.channel, &a.0.instance, &a.0.agent).cmp(&(b.0.channel, &b.0.instance, &b.0.agent))
        });
        if rows.is_empty() {
            out.push_str("channel_acl_denied_total{agent=\"\",channel=\"\",instance=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "channel_acl_denied_total{{agent=\"{}\",channel=\"{}\",instance=\"{}\"}} {v}\n",
                escape(&key.agent),
                key.channel,
                escape(&key.instance),
            ));
        }
    }

    out.push_str("# HELP credentials_resolve_errors_total Resolver failures by reason.\n");
    out.push_str("# TYPE credentials_resolve_errors_total counter\n");
    {
        let mut rows: Vec<_> = RESOLVE_ERRORS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| (a.0.channel, a.0.reason).cmp(&(b.0.channel, b.0.reason)));
        if rows.is_empty() {
            out.push_str("credentials_resolve_errors_total{channel=\"\",reason=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "credentials_resolve_errors_total{{channel=\"{}\",reason=\"{}\"}} {v}\n",
                key.channel, key.reason
            ));
        }
    }

    out.push_str(
        "# HELP credentials_breaker_state 0=closed, 1=half-open, 2=open per (channel, instance).\n",
    );
    out.push_str("# TYPE credentials_breaker_state gauge\n");
    {
        let mut rows: Vec<_> = BREAKER_STATE
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| (a.0.channel, &a.0.instance).cmp(&(b.0.channel, &b.0.instance)));
        if rows.is_empty() {
            out.push_str("credentials_breaker_state{channel=\"\",instance=\"\"} 0\n");
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "credentials_breaker_state{{channel=\"{}\",instance=\"{}\"}} {v}\n",
                key.channel,
                escape(&key.instance),
            ));
        }
    }

    out.push_str("# HELP credentials_boot_validation_errors_total Boot gauntlet errors by kind.\n");
    out.push_str("# TYPE credentials_boot_validation_errors_total counter\n");
    {
        let mut rows: Vec<_> = BOOT_ERRORS
            .iter()
            .map(|e| (e.key().kind, e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        if rows.is_empty() {
            out.push_str("credentials_boot_validation_errors_total{kind=\"\"} 0\n");
        }
        for (kind, v) in rows {
            out.push_str(&format!(
                "credentials_boot_validation_errors_total{{kind=\"{kind}\"}} {v}\n"
            ));
        }
    }

    out.push_str("# HELP credentials_insecure_paths_total Credential paths with insecure permissions at boot.\n");
    out.push_str("# TYPE credentials_insecure_paths_total gauge\n");
    out.push_str(&format!(
        "credentials_insecure_paths_total {}\n",
        INSECURE_PATHS.load(Ordering::Relaxed)
    ));

    out.push_str("# HELP credentials_google_token_refresh_total Google OAuth refresh outcomes.\n");
    out.push_str("# TYPE credentials_google_token_refresh_total counter\n");
    {
        let mut rows: Vec<_> = REFRESHES
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| (&a.0.account_fp, a.0.outcome).cmp(&(&b.0.account_fp, b.0.outcome)));
        if rows.is_empty() {
            out.push_str(
                "credentials_google_token_refresh_total{account_fp=\"\",outcome=\"\"} 0\n",
            );
        }
        for (key, v) in rows {
            out.push_str(&format!(
                "credentials_google_token_refresh_total{{account_fp=\"{}\",outcome=\"{}\"}} {v}\n",
                escape(&key.account_fp),
                key.outcome,
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{TELEGRAM, WHATSAPP};

    #[test]
    fn render_includes_every_metric_when_empty() {
        let body = render_prometheus();
        for name in [
            "credentials_accounts_total",
            "credentials_bindings_total",
            "channel_account_usage_total",
            "channel_acl_denied_total",
            "credentials_resolve_errors_total",
            "credentials_breaker_state",
            "credentials_boot_validation_errors_total",
            "credentials_insecure_paths_total",
            "credentials_google_token_refresh_total",
        ] {
            assert!(body.contains(&format!("# TYPE {name}")), "missing {name}");
        }
    }

    #[test]
    fn usage_counter_is_labelled() {
        inc_usage(WHATSAPP, "personal", "ana", "outbound");
        inc_usage(TELEGRAM, "ana_bot", "ana", "inbound");
        let body = render_prometheus();
        assert!(body.contains(
            "channel_account_usage_total{agent=\"ana\",channel=\"whatsapp\",direction=\"outbound\",instance=\"personal\"} "
        ));
    }

    #[test]
    fn acl_denied_counter_renders() {
        inc_acl_denied(WHATSAPP, "work", "ana");
        let body = render_prometheus();
        assert!(body.contains(
            "channel_acl_denied_total{agent=\"ana\",channel=\"whatsapp\",instance=\"work\"}"
        ));
    }

    #[test]
    fn escape_handles_quotes_and_backslash() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
