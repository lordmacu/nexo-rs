//! Phase 82.4 — schema for per-agent NATS event subscribers.
//!
//! An `EventSubscriberBinding` declares one subject pattern that
//! the runtime subscribes to; matching events are translated into
//! the standard inbound flow (`plugin.inbound.event.<source>`)
//! so the existing routing/binding/dispatch primitives apply.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What to do with an event that matches the subject pattern.
///
/// Operator-facing diagnostic enum — `#[non_exhaustive]` so a
/// future mode (e.g. `Forward`) lands as semver-minor without
/// breaking downstream pattern matches.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SynthesisMode {
    /// Render `inbound_template` (or fallback) into the inbound
    /// body so the agent sees the event content as a regular
    /// message. Default.
    #[default]
    Synthesize,
    /// Fire an agent turn with a `<event subject="..."
    /// envelope_id="..."/>` signal and no body. The agent retrieves
    /// the payload via tooling when it decides to act.
    Tick,
    /// Subscriber inactive (operator can stage YAML disabled).
    Off,
}

impl SynthesisMode {
    /// Stable wire string used in [`crate::types::EventSourceMeta`]
    /// (in `nexo-tool-meta`).
    pub fn as_str(self) -> &'static str {
        match self {
            SynthesisMode::Synthesize => "synthesize",
            SynthesisMode::Tick => "tick",
            SynthesisMode::Off => "off",
        }
    }
}

/// Behaviour when the per-binding inbound buffer is full.
///
/// `#[non_exhaustive]` — additive variants land as semver-minor.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OverflowPolicy {
    /// Drop the oldest queued event when buffer full (default).
    /// Logs `tracing::warn!`. Recent events tend to be more
    /// relevant under burst.
    #[default]
    DropOldest,
    /// Drop the newly-arrived event. Conservative alternative.
    DropNewest,
}

fn default_max_concurrency() -> u32 {
    1
}

fn default_max_buffer() -> usize {
    64
}

/// One NATS subject pattern → agent turn pipeline.
///
/// Caller-populated config (loaded from operator YAML).
/// Intentionally **not** `#[non_exhaustive]` — operators write
/// these via struct literal in tests; field additions are
/// deliberate semver-major.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventSubscriberBinding {
    /// Stable identifier within the agent's `event_subscribers`
    /// list. Used as the `<source_id>` in
    /// `plugin.inbound.event.<source_id>` republish topics + the
    /// Phase 72 turn-log marker.
    pub id: String,

    /// NATS subject pattern (`*` matches one segment, `>` matches
    /// the rest). Validated to NOT overlap with the inbound
    /// republish prefix `plugin.inbound.>` to prevent loops.
    pub subject_pattern: String,

    /// What to do with matching events.
    #[serde(default)]
    pub synthesize_inbound: SynthesisMode,

    /// Optional mustache-lite template (`{{path.to.field}}`)
    /// used to render the inbound body in `Synthesize` mode.
    /// `None` falls back to the JSON-stringified raw payload.
    #[serde(default)]
    pub inbound_template: Option<String>,

    /// Max in-flight events for this binding. `0` is rejected at
    /// validate time. Default `1` (serial).
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,

    /// Bounded buffer per binding. `0` is rejected. Default `64`.
    #[serde(default = "default_max_buffer")]
    pub max_buffer: usize,

    /// Overflow behaviour when the buffer is full.
    #[serde(default)]
    pub overflow_policy: OverflowPolicy,
}

/// Boot-time validation errors.
///
/// Operator-facing diagnostic — `#[non_exhaustive]`.
#[non_exhaustive]
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EventSubscriberConfigError {
    /// Empty `id`.
    #[error("event_subscriber binding has empty `id`")]
    EmptyId,
    /// Empty `subject_pattern`.
    #[error("event_subscriber `{id}` has empty `subject_pattern`")]
    EmptySubjectPattern {
        /// Binding id with the issue.
        id: String,
    },
    /// `subject_pattern` overlaps with `plugin.inbound.>` —
    /// would cause an infinite loop.
    #[error(
        "event_subscriber `{id}` subject_pattern `{pattern}` overlaps with `plugin.inbound.>` (loop risk)"
    )]
    LoopRiskPattern {
        /// Binding id with the issue.
        id: String,
        /// The configured pattern.
        pattern: String,
    },
    /// `max_concurrency` is `0`.
    #[error("event_subscriber `{id}` max_concurrency must be > 0 (got 0)")]
    ZeroConcurrency {
        /// Binding id with the issue.
        id: String,
    },
    /// `max_buffer` is `0`.
    #[error("event_subscriber `{id}` max_buffer must be > 0 (got 0)")]
    ZeroBuffer {
        /// Binding id with the issue.
        id: String,
    },
    /// Two bindings share the same `id` within an agent.
    #[error("event_subscriber duplicate id `{0}`")]
    DuplicateId(String),
}

impl EventSubscriberBinding {
    /// Validate one binding in isolation. Caller is expected to
    /// also check uniqueness across the agent's full list.
    pub fn validate(&self) -> Result<(), EventSubscriberConfigError> {
        if self.id.trim().is_empty() {
            return Err(EventSubscriberConfigError::EmptyId);
        }
        if self.subject_pattern.trim().is_empty() {
            return Err(EventSubscriberConfigError::EmptySubjectPattern {
                id: self.id.clone(),
            });
        }
        if pattern_loops_to_inbound(&self.subject_pattern) {
            return Err(EventSubscriberConfigError::LoopRiskPattern {
                id: self.id.clone(),
                pattern: self.subject_pattern.clone(),
            });
        }
        if self.max_concurrency == 0 {
            return Err(EventSubscriberConfigError::ZeroConcurrency {
                id: self.id.clone(),
            });
        }
        if self.max_buffer == 0 {
            return Err(EventSubscriberConfigError::ZeroBuffer {
                id: self.id.clone(),
            });
        }
        Ok(())
    }
}

/// Validate uniqueness of `id` across an agent's
/// `event_subscribers` list. Returns the duplicate id when found.
pub fn check_event_subscribers_unique(
    list: &[EventSubscriberBinding],
) -> Result<(), EventSubscriberConfigError> {
    let mut seen = std::collections::BTreeSet::new();
    for b in list {
        if !seen.insert(b.id.clone()) {
            return Err(EventSubscriberConfigError::DuplicateId(b.id.clone()));
        }
    }
    Ok(())
}

/// `true` when a subject pattern overlaps with the inbound
/// republish prefix `plugin.inbound.>`. We reject these at
/// validate time to prevent the event-subscriber loop where the
/// runtime publishes to `plugin.inbound.event.<source>` and then
/// receives that very publish back.
fn pattern_loops_to_inbound(pattern: &str) -> bool {
    // Direct match.
    if pattern == ">" {
        return true;
    }
    if pattern.starts_with("plugin.inbound") {
        return true;
    }
    // `plugin.>` would also match.
    if pattern == "plugin.>" || pattern == "plugin.*.>" {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: &str, pattern: &str) -> EventSubscriberBinding {
        EventSubscriberBinding {
            id: id.into(),
            subject_pattern: pattern.into(),
            synthesize_inbound: SynthesisMode::Synthesize,
            inbound_template: None,
            max_concurrency: 1,
            max_buffer: 64,
            overflow_policy: OverflowPolicy::DropOldest,
        }
    }

    #[test]
    fn defaults_validate_clean() {
        mk("a", "webhook.x.>").validate().unwrap();
    }

    #[test]
    fn empty_id_rejected() {
        assert_eq!(
            mk("", "x").validate(),
            Err(EventSubscriberConfigError::EmptyId)
        );
    }

    #[test]
    fn empty_pattern_rejected() {
        assert_eq!(
            mk("a", "").validate(),
            Err(EventSubscriberConfigError::EmptySubjectPattern { id: "a".into() })
        );
    }

    #[test]
    fn loop_risk_patterns_rejected() {
        for bad in [">", "plugin.>", "plugin.*.>", "plugin.inbound.event.x"] {
            let result = mk("a", bad).validate();
            assert!(
                matches!(result, Err(EventSubscriberConfigError::LoopRiskPattern { .. })),
                "pattern `{bad}` should be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn zero_concurrency_rejected() {
        let mut b = mk("a", "webhook.>");
        b.max_concurrency = 0;
        assert_eq!(
            b.validate(),
            Err(EventSubscriberConfigError::ZeroConcurrency { id: "a".into() })
        );
    }

    #[test]
    fn zero_buffer_rejected() {
        let mut b = mk("a", "webhook.>");
        b.max_buffer = 0;
        assert_eq!(
            b.validate(),
            Err(EventSubscriberConfigError::ZeroBuffer { id: "a".into() })
        );
    }

    #[test]
    fn duplicate_ids_detected() {
        let list = vec![mk("a", "x"), mk("a", "y")];
        assert_eq!(
            check_event_subscribers_unique(&list),
            Err(EventSubscriberConfigError::DuplicateId("a".into()))
        );
    }

    #[test]
    fn three_synthesis_modes_round_trip_via_yaml() {
        for (mode, expected) in [
            (SynthesisMode::Synthesize, "synthesize"),
            (SynthesisMode::Tick, "tick"),
            (SynthesisMode::Off, "off"),
        ] {
            let yaml = serde_yaml::to_string(&mode).unwrap();
            assert_eq!(yaml.trim(), expected);
            let back: SynthesisMode = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(back, mode);
            assert_eq!(mode.as_str(), expected);
        }
    }

    #[test]
    fn full_yaml_round_trip() {
        let yaml = r#"
id: "github_main"
subject_pattern: "webhook.github_main.>"
synthesize_inbound: "synthesize"
inbound_template: "GitHub {{event_kind}}: {{body_json.action}}"
max_concurrency: 4
max_buffer: 128
overflow_policy: "drop-newest"
"#;
        let b: EventSubscriberBinding = serde_yaml::from_str(yaml).unwrap();
        b.validate().unwrap();
        assert_eq!(b.id, "github_main");
        assert_eq!(b.synthesize_inbound, SynthesisMode::Synthesize);
        assert_eq!(b.overflow_policy, OverflowPolicy::DropNewest);
        assert_eq!(b.max_concurrency, 4);
        assert!(b.inbound_template.is_some());
    }

    #[test]
    fn defaults_apply_when_optional_fields_omitted() {
        let yaml = r#"
id: "github_main"
subject_pattern: "webhook.github_main.>"
"#;
        let b: EventSubscriberBinding = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(b.synthesize_inbound, SynthesisMode::Synthesize);
        assert_eq!(b.overflow_policy, OverflowPolicy::DropOldest);
        assert_eq!(b.max_concurrency, 1);
        assert_eq!(b.max_buffer, 64);
        assert!(b.inbound_template.is_none());
    }
}
