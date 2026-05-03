//! Phase 88.1 — `nexo/admin/pollers/*` wire types.
//!
//! Microapp-facing surface for managing periodic tasks
//! (cron / interval / one-shot) without dropping to YAML
//! editing. Mirrors the on-disk `config/poller.yaml` schema
//! (owned by `nexo-config::types::pollers`) but kept independent
//! so this crate has zero dep direction back into `nexo-config`.
//!
//! The runtime is `nexo-poller`; this module describes the
//! admin RPC contract. Eight admin methods plus the runtime
//! status read-back fields populated from `nexo-poller`'s
//! `JobView`:
//!
//! - `pollers/list`     ← read-only, capability `pollers_read`
//! - `pollers/get`      ← read-only, capability `pollers_read`
//! - `pollers/upsert`   ← yaml mutation, capability `pollers_crud`
//! - `pollers/delete`   ← yaml mutation, capability `pollers_crud`
//! - `pollers/pause`    ← runtime control, capability `pollers_runtime`
//! - `pollers/resume`   ← runtime control, capability `pollers_runtime`
//! - `pollers/run_now`  ← runtime control, capability `pollers_runtime`
//!
//! ## Schedule shape
//!
//! [`PollerSchedule`] is untagged so the JSON / YAML
//! discriminator is the field name (`every_secs` / `cron` /
//! `at`), matching `nexo-poller::schedule::Schedule` exactly.
//!
//! ## Config opacity
//!
//! [`PollerEntry::config`] is `serde_json::Value` rather than a
//! per-kind typed enum. Five kinds ship today (`agent_turn`,
//! `gmail`, `google_calendar`, `rss`, `webhook_poll`) and the
//! kind catalogue is `#[non_exhaustive]` at the runtime level —
//! typing the wire shape per-kind would force a breaking change
//! every time a new kind lands. Operator UIs render kind-specific
//! editors client-side; the wire stays generic.

use serde::{Deserialize, Serialize};

// ── Method literals ─────────────────────────────────────────

/// JSON-RPC method literal for `pollers/list`.
pub const POLLERS_LIST_METHOD: &str = "nexo/admin/pollers/list";
/// JSON-RPC method literal for `pollers/get`.
pub const POLLERS_GET_METHOD: &str = "nexo/admin/pollers/get";
/// JSON-RPC method literal for `pollers/upsert`.
pub const POLLERS_UPSERT_METHOD: &str = "nexo/admin/pollers/upsert";
/// JSON-RPC method literal for `pollers/delete`.
pub const POLLERS_DELETE_METHOD: &str = "nexo/admin/pollers/delete";
/// JSON-RPC method literal for `pollers/pause`.
pub const POLLERS_PAUSE_METHOD: &str = "nexo/admin/pollers/pause";
/// JSON-RPC method literal for `pollers/resume`.
pub const POLLERS_RESUME_METHOD: &str = "nexo/admin/pollers/resume";
/// JSON-RPC method literal for `pollers/run_now`.
pub const POLLERS_RUN_NOW_METHOD: &str = "nexo/admin/pollers/run_now";

// ── Constants ──────────────────────────────────────────────

/// Regex pattern for poller `id` — snake_case identifier
/// matching `^[a-z][a-z0-9_]{1,63}$`. Same convention the
/// existing `nexo-config::types::pollers::PollerJob.id` uses
/// in production yaml. The handler validates client input
/// against this pattern.
pub const POLLER_ID_REGEX: &str = r"^[a-z][a-z0-9_]{1,63}$";

/// Maximum length for `failure_to.to` (channel recipient).
/// Phone numbers, chat IDs, and email addresses all fit
/// comfortably under 256.
pub const FAILURE_RECIPIENT_MAX_LEN: usize = 256;

// ── Schedule shape ─────────────────────────────────────────

/// One of three scheduling variants. Untagged: the field name
/// (`every_secs` / `cron` / `at`) discriminates which variant
/// the YAML / JSON parser picks. Mirror of
/// `nexo-poller::schedule::Schedule`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PollerSchedule {
    /// Fire every N seconds. Optional jitter to avoid thundering
    /// herd when many jobs share a period.
    Every {
        /// Period in seconds. Runner clamps to `>= 1`.
        every_secs: u64,
        /// Random offset (ms) added to each `next_run_at`.
        /// `None` = use runner-wide default jitter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stagger_jitter_ms: Option<u64>,
    },
    /// Fire on a cron expression. 6-field format
    /// (`sec min hour day-of-month month day-of-week`).
    Cron {
        /// 6-field cron expression.
        cron: String,
        /// IANA timezone (e.g. `America/Bogota`). Default UTC.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tz: Option<String>,
        /// Random offset (ms) added to each `next_run_at`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stagger_jitter_ms: Option<u64>,
    },
    /// Fire once at the supplied RFC3339 timestamp. Job stays
    /// paused after firing — no re-trigger.
    At {
        /// RFC3339 timestamp.
        at: String,
    },
}

// ── Delivery target ────────────────────────────────────────

/// Where the runner ships failure alerts after the per-job
/// circuit breaker trips. Mirror of
/// `nexo-config::types::pollers::DeliveryTarget`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeliveryTargetWire {
    /// Channel id — `whatsapp` | `telegram` | `google` | future.
    pub channel: String,
    /// Recipient identifier. Audited as a hash; full value
    /// flows to the store.
    pub to: String,
}

// ── Entry shape (read-back) ────────────────────────────────

/// One poller entry returned by `list` / `get` / `upsert`.
/// Carries both the static config (id, kind, agent, schedule,
/// config blob, failure_to, paused_on_boot) AND the runtime
/// status fields populated from `nexo-poller`'s `JobView`.
///
/// On `upsert` the runtime fields will reflect the
/// post-reload state: `paused` mirrors the `paused_on_boot`
/// flag for new jobs; existing jobs preserve their live
/// state across the upsert (no implicit resume).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollerEntry {
    /// Unique identifier — used for state, metrics, admin
    /// endpoints. Validated against [`POLLER_ID_REGEX`].
    pub id: String,
    /// Discriminator matching `Poller::kind()`. Five builtin
    /// kinds: `agent_turn` | `gmail` | `google_calendar` |
    /// `rss` | `webhook_poll`. Future kinds extend this.
    pub kind: String,
    /// Agent whose Phase 17 credentials this job uses.
    /// Cross-checked against `agents.yaml` at upsert time.
    pub agent: String,
    /// Schedule expression.
    pub schedule: PollerSchedule,
    /// Module-specific config blob. Validated by the kind's
    /// own parser at runtime; the wire stays opaque so new
    /// kinds don't bump the schema.
    #[serde(default)]
    pub config: serde_json::Value,
    /// Optional failure-alert delivery target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_to: Option<DeliveryTargetWire>,
    /// When `true`, runner does NOT auto-spawn the job at
    /// boot. Operator can later flip via `resume`.
    #[serde(default)]
    pub paused_on_boot: bool,

    // ── Runtime fields (read-only) ──
    /// True when the job is currently paused at runtime
    /// (regardless of `paused_on_boot`).
    pub paused: bool,
    /// Epoch ms of the last completed tick. `None` for
    /// never-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at_ms: Option<i64>,
    /// Epoch ms of the next scheduled tick. `None` when the
    /// schedule is exhausted (At-style after firing) or the
    /// job is paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at_ms: Option<i64>,
    /// Last tick's outcome label (`ok` / `transient_error` /
    /// `permanent_error`). `None` for never-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    /// Last tick's error message when `last_status` is an
    /// error variant. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Number of consecutive failed ticks. Resets on success.
    /// Crosses `breaker_threshold` → circuit opens.
    pub consecutive_errors: i64,
    /// Total items the job has observed (kind-specific
    /// counter — emails seen, RSS entries scanned, etc.).
    pub items_seen_total: i64,
    /// Total items the job has actually dispatched downstream
    /// (after dedup / filtering).
    pub items_dispatched_total: i64,
}

// ── List ──────────────────────────────────────────────────

/// Filter for `pollers/list`. Both filters AND together when
/// both supplied.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PollersListFilter {
    /// When set, only return pollers whose `agent` matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// When set, only return pollers of this kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Response for `pollers/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PollersListResponse {
    /// Matching pollers in stable order (alpha by `id`).
    pub pollers: Vec<PollerEntry>,
}

// ── Get ───────────────────────────────────────────────────

/// Params for `pollers/get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollersGetParams {
    /// Poller id.
    pub id: String,
}

// ── Upsert ────────────────────────────────────────────────

/// Params for `pollers/upsert`. Excludes runtime fields —
/// the handler populates them from `JobView` on response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollersUpsertInput {
    /// Unique identifier. Validated against
    /// [`POLLER_ID_REGEX`]. Existing id → update; new id →
    /// insert. Idempotent.
    pub id: String,
    /// Discriminator. Validated against the runtime kind
    /// catalogue server-side.
    pub kind: String,
    /// Agent whose credentials this job uses. Cross-checked
    /// against `agents.yaml` server-side.
    pub agent: String,
    /// Schedule expression.
    pub schedule: PollerSchedule,
    /// Module-specific config. `None` defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Optional failure-alert delivery target. Recipient
    /// length validated against [`FAILURE_RECIPIENT_MAX_LEN`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_to: Option<DeliveryTargetWire>,
    /// Spawn paused on boot.
    #[serde(default)]
    pub paused_on_boot: bool,
}

/// Response for `pollers/upsert`. Returns the updated entry
/// with runtime fields populated post-reload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollersUpsertResponse {
    /// The upserted poller, including runtime status.
    pub entry: PollerEntry,
    /// `true` when the upsert created a new job; `false`
    /// when it replaced an existing one. Lets the UI surface
    /// "created" vs "updated" toasts.
    pub created: bool,
}

// ── Delete ────────────────────────────────────────────────

/// Params for `pollers/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollersDeleteParams {
    /// Poller id.
    pub id: String,
}

/// Response for `pollers/delete`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PollersDeleteResponse {
    /// `true` when the entry was removed; `false` when it
    /// was already absent (idempotent delete).
    pub removed: bool,
}

// ── Runtime control ───────────────────────────────────────

/// Params for `pollers/pause` / `pollers/resume` /
/// `pollers/run_now`. Single shared shape because all three
/// operate on a poller id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollersRuntimeParams {
    /// Poller id.
    pub id: String,
}

/// Response for runtime control methods.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollersRuntimeResponse {
    /// Echoed poller id.
    pub id: String,
    /// `true` when the operation actually changed state
    /// (e.g. `pause` on a running job). `false` when it was
    /// a no-op (e.g. `pause` on an already-paused job, or
    /// `run_now` on a paused job that the runner refused).
    pub applied: bool,
    /// Resulting runtime state (`running` / `paused` /
    /// `errored` / `running_now`). The UI uses this to
    /// refresh the badge without re-fetching the list.
    pub new_state: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn method_literals_match_expected_jsonrpc_paths() {
        assert_eq!(POLLERS_LIST_METHOD, "nexo/admin/pollers/list");
        assert_eq!(POLLERS_GET_METHOD, "nexo/admin/pollers/get");
        assert_eq!(POLLERS_UPSERT_METHOD, "nexo/admin/pollers/upsert");
        assert_eq!(POLLERS_DELETE_METHOD, "nexo/admin/pollers/delete");
        assert_eq!(POLLERS_PAUSE_METHOD, "nexo/admin/pollers/pause");
        assert_eq!(POLLERS_RESUME_METHOD, "nexo/admin/pollers/resume");
        assert_eq!(POLLERS_RUN_NOW_METHOD, "nexo/admin/pollers/run_now");
    }

    #[test]
    fn id_regex_pattern_is_locked() {
        // Defensive: the pattern is wire-stable. Bumping it
        // requires intentional schema change + microapp UI
        // validation update.
        assert_eq!(POLLER_ID_REGEX, r"^[a-z][a-z0-9_]{1,63}$");
    }

    #[test]
    fn schedule_every_round_trips() {
        let s = PollerSchedule::Every {
            every_secs: 60,
            stagger_jitter_ms: Some(2_000),
        };
        let v = serde_json::to_value(&s).unwrap();
        // Untagged: surface fields directly.
        assert_eq!(v["every_secs"], 60);
        assert_eq!(v["stagger_jitter_ms"], 2_000);
        assert!(v.get("cron").is_none(), "cron field absent on Every");
        let back: PollerSchedule = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn schedule_every_omits_jitter_when_none() {
        let s = PollerSchedule::Every {
            every_secs: 30,
            stagger_jitter_ms: None,
        };
        let txt = serde_json::to_string(&s).unwrap();
        assert!(!txt.contains("stagger_jitter_ms"));
        let back: PollerSchedule = serde_json::from_str(&txt).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn schedule_cron_round_trips_with_tz() {
        let s = PollerSchedule::Cron {
            cron: "0 0 8 * * *".into(),
            tz: Some("America/Bogota".into()),
            stagger_jitter_ms: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["cron"], "0 0 8 * * *");
        assert_eq!(v["tz"], "America/Bogota");
        let back: PollerSchedule = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn schedule_at_round_trips() {
        let s = PollerSchedule::At {
            at: "2026-12-31T23:59:59Z".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["at"], "2026-12-31T23:59:59Z");
        let back: PollerSchedule = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn entry_round_trips_with_runtime_fields() {
        let e = PollerEntry {
            id: "ana_email_leads".into(),
            kind: "gmail".into(),
            agent: "ana".into(),
            schedule: PollerSchedule::Every {
                every_secs: 600,
                stagger_jitter_ms: None,
            },
            config: json!({ "query": "is:unread" }),
            failure_to: Some(DeliveryTargetWire {
                channel: "telegram".into(),
                to: "1194292426".into(),
            }),
            paused_on_boot: false,
            paused: false,
            last_run_at_ms: Some(1_700_000_000_000),
            next_run_at_ms: Some(1_700_000_600_000),
            last_status: Some("ok".into()),
            last_error: None,
            consecutive_errors: 0,
            items_seen_total: 42,
            items_dispatched_total: 7,
        };
        let v = serde_json::to_value(&e).unwrap();
        // None fields must skip on the wire.
        assert!(v.get("last_error").is_none());
        let back: PollerEntry = serde_json::from_value(v).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn upsert_input_round_trip_with_minimal_config() {
        let i = PollersUpsertInput {
            id: "etb_lead_router".into(),
            kind: "agent_turn".into(),
            agent: "etb_lead_router".into(),
            schedule: PollerSchedule::Every {
                every_secs: 300,
                stagger_jitter_ms: None,
            },
            config: None,
            failure_to: None,
            paused_on_boot: true,
        };
        let v = serde_json::to_value(&i).unwrap();
        // None config + None failure_to skipped on the wire.
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("config"));
        assert!(!obj.contains_key("failure_to"));
        assert_eq!(v["paused_on_boot"], true);
        let back: PollersUpsertInput = serde_json::from_value(v).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn list_filter_defaults_skip_on_wire() {
        let f = PollersListFilter::default();
        let v = serde_json::to_value(&f).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.is_empty(), "default filter serializes to {{}}");

        let f2 = PollersListFilter {
            agent_id: Some("ana".into()),
            kind: None,
        };
        let v2 = serde_json::to_value(&f2).unwrap();
        assert_eq!(v2["agent_id"], "ana");
        assert!(v2.get("kind").is_none());
    }

    #[test]
    fn runtime_response_carries_state_label() {
        let r = PollersRuntimeResponse {
            id: "ana_email_leads".into(),
            applied: true,
            new_state: "paused".into(),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["new_state"], "paused");
        let back: PollersRuntimeResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r, back);
    }
}
