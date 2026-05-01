//! Hot-reload primitives — re-evaluate the router state against
//! a freshly-loaded YAML snapshot. Returns a typed delta the
//! caller logs / surfaces in setup-doctor / admin UI.
//!
//! Phase 18 post-reload hook calls `reevaluate(router_state, &new_cfg)`
//! and uses the report to swap an `arc_swap::ArcSwap` cell that
//! holds the live router. Atomic from the request handler's
//! perspective — already-in-flight requests finish on the old
//! state; new requests pick up the new one.
//!
//! Build a *new* `Router` via `build_router` for the new config;
//! this module focuses on the *delta* (which sources are
//! kept/added/evicted) and the typed reasons surfaced in tracing.

use std::collections::HashSet;
use std::sync::Arc;

use nexo_config::types::webhook_receiver::WebhookServerConfig;

use crate::router::RouterState;

/// Why a previously-mounted source no longer survives the new
/// config snapshot.
///
/// `#[non_exhaustive]` — operator-facing diagnostic; future
/// reasons land as semver-minor.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvictionReason {
    /// Source absent from the new YAML.
    Removed,
    /// New YAML has `enabled: false` — global killswitch flip.
    Disabled,
    /// Source remapped to a different path (treat as remove +
    /// add for routing purposes).
    PathChanged {
        /// Path the source was previously mounted at.
        old: String,
        /// Path declared in the new YAML.
        new: String,
    },
    /// Per-source signature algorithm / header / secret_env
    /// changed — previous handler is invalidated; rebuild needed.
    SignatureChanged,
}

/// One row in [`ReevaluateReport::evicted`].
#[derive(Debug, Clone)]
pub struct EvictedSource {
    /// Webhook source id that was unmounted.
    pub source_id: String,
    /// Why the source was unmounted.
    pub reason: EvictionReason,
}

/// Delta surfaced after a reload. `kept` keeps existing
/// `SourceState` Arc references so live request handlers don't
/// see a stale rebuild for unchanged sources (semaphore + bucket
/// continuity).
#[derive(Debug, Clone)]
pub struct ReevaluateReport {
    /// Source ids kept untouched (config matched the previous
    /// snapshot).
    pub kept: Vec<String>,
    /// Source ids declared in the new config that were absent
    /// previously OR that survived a path/signature rebuild.
    pub added: Vec<String>,
    /// Sources unmounted, with the typed reason.
    pub evicted: Vec<EvictedSource>,
}

impl ReevaluateReport {
    /// `true` when the report contains no `added` or `evicted`
    /// rows — short-circuits noisy logs on idempotent reloads.
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.evicted.is_empty()
    }
}

/// Compute the kept/added/evicted delta between the previous
/// `RouterState` and a fresh `WebhookServerConfig` snapshot.
///
/// Caller is expected to invoke `build_router(new_cfg, dispatcher)`
/// separately to get the new `Router` + `RouterState`; this
/// function does NOT mutate anything — it's a pure-fn comparator
/// so the caller can swap atomically via `ArcSwap`.
pub fn reevaluate(
    previous: &Arc<RouterState>,
    new_cfg: &WebhookServerConfig,
) -> ReevaluateReport {
    let mut kept = Vec::new();
    let mut added = Vec::new();
    let mut evicted = Vec::new();

    // Killswitch flip → every previously-mounted source evicts.
    if !new_cfg.enabled {
        let mut all: Vec<String> = previous.sources.keys().cloned().collect();
        all.sort();
        for id in all {
            evicted.push(EvictedSource {
                source_id: id,
                reason: EvictionReason::Disabled,
            });
        }
        return ReevaluateReport {
            kept,
            added,
            evicted,
        };
    }

    let mut new_ids: HashSet<String> = HashSet::new();
    for s in &new_cfg.sources {
        new_ids.insert(s.source.id.clone());
    }

    // Walk previous → kept (still present, same shape) or
    // evicted (removed / shape changed).
    let mut prev_ids: Vec<String> = previous.sources.keys().cloned().collect();
    prev_ids.sort();
    for id in &prev_ids {
        let prev_state = &previous.sources[id];
        let new_match = new_cfg.sources.iter().find(|s| s.source.id == *id);
        match new_match {
            None => evicted.push(EvictedSource {
                source_id: id.clone(),
                reason: EvictionReason::Removed,
            }),
            Some(new_s) => {
                if new_s.source.path != prev_state.path {
                    evicted.push(EvictedSource {
                        source_id: id.clone(),
                        reason: EvictionReason::PathChanged {
                            old: prev_state.path.clone(),
                            new: new_s.source.path.clone(),
                        },
                    });
                    // Path-changed entries also count as "added"
                    // under the new path so the upstream rebuild
                    // logs both events.
                    added.push(id.clone());
                } else if signature_changed(prev_state, new_s) {
                    evicted.push(EvictedSource {
                        source_id: id.clone(),
                        reason: EvictionReason::SignatureChanged,
                    });
                    added.push(id.clone());
                } else {
                    kept.push(id.clone());
                }
            }
        }
    }

    // Walk new → flag any not seen in previous as added.
    for s in &new_cfg.sources {
        if !previous.sources.contains_key(&s.source.id)
            && !added.contains(&s.source.id)
        {
            added.push(s.source.id.clone());
        }
    }

    // Stable ordering for tests + deterministic logs.
    kept.sort();
    added.sort();
    evicted.sort_by(|a, b| a.source_id.cmp(&b.source_id));

    ReevaluateReport {
        kept,
        added,
        evicted,
    }
}

fn signature_changed(
    prev: &crate::router::SourceState,
    new_s: &nexo_config::types::webhook_receiver::WebhookSourceWithLimits,
) -> bool {
    let prev_cfg = prev.handler.config();
    prev_cfg.signature.algorithm != new_s.source.signature.algorithm
        || prev_cfg.signature.header != new_s.source.signature.header
        || prev_cfg.signature.secret_env != new_s.source.signature.secret_env
        || prev_cfg.signature.prefix != new_s.source.signature.prefix
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::build_router;
    use nexo_config::types::webhook_receiver::WebhookSourceWithLimits;
    use nexo_webhook_receiver::{
        EventKindSource, RecordingWebhookDispatcher, SignatureAlgorithm, SignatureSpec,
        WebhookSourceConfig,
    };

    fn mk_source(id: &str, path: &str, secret_env: &str) -> WebhookSourceWithLimits {
        WebhookSourceWithLimits {
            source: WebhookSourceConfig {
                id: id.into(),
                path: path.into(),
                signature: SignatureSpec {
                    algorithm: SignatureAlgorithm::HmacSha256,
                    header: "X-Sig".into(),
                    prefix: "sha256=".into(),
                    secret_env: secret_env.into(),
                },
                publish_to: format!("webhook.{id}.${{event_kind}}"),
                event_kind_from: EventKindSource::Header {
                    name: "X-Event".into(),
                },
                body_cap_bytes: None,
            },
            rate_limit: None,
            concurrency_cap: None,
        }
    }

    fn mk_cfg(sources: Vec<WebhookSourceWithLimits>) -> WebhookServerConfig {
        WebhookServerConfig {
            enabled: true,
            sources,
            ..Default::default()
        }
    }

    fn previous_state(cfg: &WebhookServerConfig) -> Arc<RouterState> {
        std::env::set_var("WEBHOOK_TEST_RELOAD_SECRET", "x");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (_router, state) = build_router(cfg, dispatcher).unwrap();
        state
    }

    #[test]
    fn unchanged_config_keeps_all_sources() {
        let cfg = mk_cfg(vec![
            mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET"),
            mk_source("b", "/b", "WEBHOOK_TEST_RELOAD_SECRET"),
        ]);
        let prev = previous_state(&cfg);
        let report = reevaluate(&prev, &cfg);
        assert_eq!(report.kept, vec!["a".to_string(), "b".to_string()]);
        assert!(report.added.is_empty());
        assert!(report.evicted.is_empty());
        assert!(report.is_noop());
    }

    #[test]
    fn removed_source_evicts() {
        let prev_cfg = mk_cfg(vec![
            mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET"),
            mk_source("b", "/b", "WEBHOOK_TEST_RELOAD_SECRET"),
        ]);
        let prev = previous_state(&prev_cfg);
        let new_cfg = mk_cfg(vec![mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET")]);
        let report = reevaluate(&prev, &new_cfg);
        assert_eq!(report.kept, vec!["a".to_string()]);
        assert!(report.added.is_empty());
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].source_id, "b");
        assert_eq!(report.evicted[0].reason, EvictionReason::Removed);
    }

    #[test]
    fn added_source_listed() {
        let prev_cfg = mk_cfg(vec![mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET")]);
        let prev = previous_state(&prev_cfg);
        let new_cfg = mk_cfg(vec![
            mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET"),
            mk_source("c", "/c", "WEBHOOK_TEST_RELOAD_SECRET"),
        ]);
        let report = reevaluate(&prev, &new_cfg);
        assert_eq!(report.kept, vec!["a".to_string()]);
        assert_eq!(report.added, vec!["c".to_string()]);
        assert!(report.evicted.is_empty());
    }

    #[test]
    fn killswitch_off_evicts_everything() {
        let prev_cfg = mk_cfg(vec![
            mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET"),
            mk_source("b", "/b", "WEBHOOK_TEST_RELOAD_SECRET"),
        ]);
        let prev = previous_state(&prev_cfg);
        let mut new_cfg = prev_cfg.clone();
        new_cfg.enabled = false;
        let report = reevaluate(&prev, &new_cfg);
        assert!(report.kept.is_empty());
        assert!(report.added.is_empty());
        assert_eq!(report.evicted.len(), 2);
        for e in &report.evicted {
            assert_eq!(e.reason, EvictionReason::Disabled);
        }
    }

    #[test]
    fn path_change_evicts_then_adds() {
        let prev_cfg = mk_cfg(vec![mk_source("a", "/old", "WEBHOOK_TEST_RELOAD_SECRET")]);
        let prev = previous_state(&prev_cfg);
        let new_cfg = mk_cfg(vec![mk_source("a", "/new", "WEBHOOK_TEST_RELOAD_SECRET")]);
        let report = reevaluate(&prev, &new_cfg);
        assert!(report.kept.is_empty());
        assert_eq!(report.added, vec!["a".to_string()]);
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(
            report.evicted[0].reason,
            EvictionReason::PathChanged {
                old: "/old".into(),
                new: "/new".into()
            }
        );
    }

    #[test]
    fn signature_change_evicts_then_adds() {
        let prev_cfg = mk_cfg(vec![mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET_OLD")]);
        let prev = previous_state(&prev_cfg);
        let new_cfg = mk_cfg(vec![mk_source("a", "/a", "WEBHOOK_TEST_RELOAD_SECRET_NEW")]);
        let report = reevaluate(&prev, &new_cfg);
        assert!(report.kept.is_empty());
        assert_eq!(report.added, vec!["a".to_string()]);
        assert_eq!(report.evicted[0].reason, EvictionReason::SignatureChanged);
    }
}
