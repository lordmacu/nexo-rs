//! Phase 19 — translate legacy `config/plugins/gmail-poller.yaml`
//! shape into the generic `PollersConfig` consumed by
//! `crates/poller`. Lets existing deployments keep their YAML while
//! the runtime loop is now the new generic runner.
//!
//! Boot warns once per translated job so operators know to migrate.

use agent_config::types::pollers::{DeliveryTarget, PollerJob, PollersConfig};
use serde_yaml::{Mapping, Value as YamlValue};

#[cfg(test)]
use crate::config::JobConfig;
use crate::config::GmailPollerConfig;

/// Translate a legacy `gmail-poller.yaml` into a `PollersConfig` that
/// the generic poller subsystem can consume. The result can be merged
/// with whatever the operator declared in `pollers.yaml` directly.
///
/// Every job emits a `tracing::warn` so the deprecation is visible.
pub fn translate(legacy: &GmailPollerConfig) -> PollersConfig {
    if !legacy.enabled {
        return PollersConfig::default();
    }

    let accounts = match crate::resolve_accounts_for_translate(legacy) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "gmail-poller legacy translate skipped");
            return PollersConfig::default();
        }
    };
    // Account id → agent id (defaults to account id when unset).
    let agent_for: std::collections::HashMap<String, String> = accounts
        .iter()
        .map(|a| (a.id.clone(), a.agent_id.clone().unwrap_or_else(|| a.id.clone())))
        .collect();

    let mut jobs: Vec<PollerJob> = Vec::with_capacity(legacy.jobs.len());
    let default_interval = legacy.interval_secs.max(1);
    for j in &legacy.jobs {
        let Some(agent) = agent_for.get(&j.account).cloned() else {
            tracing::warn!(
                job = %j.name,
                account = %j.account,
                "gmail-poller legacy job references unknown account; skipping translation"
            );
            continue;
        };
        let interval = j.interval_secs.unwrap_or(default_interval).max(1);
        let (channel, _instance) = parse_forward_subject(&j.forward_to_subject);
        let deliver = DeliveryTarget {
            channel: channel.clone(),
            to: j.forward_to.clone(),
        };

        let mut module_cfg = Mapping::new();
        module_cfg.insert(YamlValue::String("query".into()), YamlValue::String(j.query.clone()));
        if let Some(nt) = &j.newer_than {
            module_cfg.insert(
                YamlValue::String("newer_than".into()),
                YamlValue::String(nt.clone()),
            );
        }
        module_cfg.insert(
            YamlValue::String("max_per_tick".into()),
            YamlValue::Number((j.max_per_tick as i64).into()),
        );
        module_cfg.insert(
            YamlValue::String("dispatch_delay_ms".into()),
            YamlValue::Number((j.dispatch_delay_ms as i64).into()),
        );
        let allowlist: Vec<YamlValue> = j
            .sender_allowlist
            .iter()
            .map(|s| YamlValue::String(s.clone()))
            .collect();
        module_cfg.insert(
            YamlValue::String("sender_allowlist".into()),
            YamlValue::Sequence(allowlist),
        );
        let mut extract = Mapping::new();
        for (k, v) in &j.extract {
            extract.insert(YamlValue::String(k.clone()), YamlValue::String(v.clone()));
        }
        module_cfg.insert(YamlValue::String("extract".into()), YamlValue::Mapping(extract));
        let req: Vec<YamlValue> = j
            .require_fields
            .iter()
            .map(|s| YamlValue::String(s.clone()))
            .collect();
        module_cfg.insert(
            YamlValue::String("require_fields".into()),
            YamlValue::Sequence(req),
        );
        module_cfg.insert(
            YamlValue::String("message_template".into()),
            YamlValue::String(j.message_template.clone()),
        );
        module_cfg.insert(
            YamlValue::String("mark_read_on_dispatch".into()),
            YamlValue::Bool(j.mark_read_on_dispatch),
        );
        let mut deliver_map = Mapping::new();
        deliver_map.insert(
            YamlValue::String("channel".into()),
            YamlValue::String(deliver.channel.clone()),
        );
        deliver_map.insert(
            YamlValue::String("to".into()),
            YamlValue::String(deliver.to.clone()),
        );
        module_cfg.insert(
            YamlValue::String("deliver".into()),
            YamlValue::Mapping(deliver_map),
        );

        // Schedule: every N seconds.
        let mut schedule = Mapping::new();
        schedule.insert(
            YamlValue::String("every_secs".into()),
            YamlValue::Number((interval as i64).into()),
        );

        let id = format!("gmail_legacy_{}", sanitize_id(&j.name));
        tracing::warn!(
            job = %j.name,
            translated_id = %id,
            "gmail-poller legacy: migrate this job to config/pollers.yaml as `kind: gmail`"
        );

        jobs.push(PollerJob {
            id,
            kind: "gmail".into(),
            agent,
            schedule: YamlValue::Mapping(schedule),
            config: YamlValue::Mapping(module_cfg),
            failure_to: None,
            paused_on_boot: false,
            extra: Default::default(),
        });
    }

    PollersConfig {
        jobs,
        ..PollersConfig::default()
    }
}

/// Parse `plugin.outbound.<channel>.<instance>` into `(channel, instance)`.
/// Returns `(channel, "default")` for the legacy un-suffixed shape.
fn parse_forward_subject(subject: &str) -> (String, String) {
    let parts: Vec<&str> = subject.split('.').collect();
    match parts.as_slice() {
        ["plugin", "outbound", channel, rest @ ..] if !rest.is_empty() => {
            (channel.to_string(), rest.join("."))
        }
        ["plugin", "outbound", channel] => (channel.to_string(), "default".to_string()),
        _ => ("whatsapp".to_string(), "default".to_string()),
    }
}

/// Sanitize a job name to a YAML/SQLite-friendly job id.
fn sanitize_id(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountConfig;

    fn job(name: &str, account: &str) -> JobConfig {
        JobConfig {
            name: name.into(),
            query: "is:unread".into(),
            interval_secs: Some(60),
            forward_to_subject: "plugin.outbound.whatsapp.personal".into(),
            forward_to: "57300@s.whatsapp.net".into(),
            extract: Default::default(),
            message_template: "{snippet}".into(),
            mark_read_on_dispatch: true,
            require_fields: vec![],
            newer_than: Some("1d".into()),
            dispatch_delay_ms: 1000,
            max_per_tick: 20,
            sender_allowlist: vec![],
            account: account.into(),
        }
    }

    #[test]
    fn parses_topic_with_instance() {
        let (c, i) = parse_forward_subject("plugin.outbound.telegram.kate_bot");
        assert_eq!(c, "telegram");
        assert_eq!(i, "kate_bot");
    }

    #[test]
    fn parses_topic_without_instance() {
        let (c, i) = parse_forward_subject("plugin.outbound.whatsapp");
        assert_eq!(c, "whatsapp");
        assert_eq!(i, "default");
    }

    #[test]
    fn translate_skips_when_disabled() {
        let cfg = GmailPollerConfig {
            enabled: false,
            interval_secs: 60,
            token_path: Some("/tmp/x".into()),
            client_id_path: None,
            client_secret_path: None,
            accounts: vec![],
            jobs: vec![job("a", "default")],
        };
        let p = translate(&cfg);
        assert!(p.jobs.is_empty());
    }

    #[test]
    fn translate_fills_kind_agent_schedule() {
        let cfg = GmailPollerConfig {
            enabled: true,
            interval_secs: 60,
            token_path: None,
            client_id_path: None,
            client_secret_path: None,
            accounts: vec![AccountConfig {
                id: "ana".into(),
                token_path: "/tmp/tok".into(),
                client_id_path: "/tmp/cid".into(),
                client_secret_path: "/tmp/csec".into(),
                agent_id: Some("ana".into()),
            }],
            jobs: vec![job("Lead Alert", "ana")],
        };
        let p = translate(&cfg);
        assert_eq!(p.jobs.len(), 1);
        let j = &p.jobs[0];
        assert_eq!(j.kind, "gmail");
        assert_eq!(j.agent, "ana");
        assert_eq!(j.id, "gmail_legacy_Lead_Alert");
        assert!(j.schedule.get("every_secs").is_some());
        // Module config should carry forward fields.
        assert_eq!(
            j.config.get("query").and_then(|v| v.as_str()),
            Some("is:unread")
        );
        assert_eq!(
            j.config
                .get("deliver")
                .and_then(|d| d.get("channel"))
                .and_then(|v| v.as_str()),
            Some("whatsapp")
        );
    }

    #[test]
    fn translate_skips_unknown_account() {
        let cfg = GmailPollerConfig {
            enabled: true,
            interval_secs: 60,
            token_path: None,
            client_id_path: None,
            client_secret_path: None,
            accounts: vec![AccountConfig {
                id: "ana".into(),
                token_path: "/tmp/tok".into(),
                client_id_path: "/tmp/cid".into(),
                client_secret_path: "/tmp/csec".into(),
                agent_id: None,
            }],
            jobs: vec![job("Bad", "nonexistent")],
        };
        let p = translate(&cfg);
        assert!(p.jobs.is_empty());
    }
}
