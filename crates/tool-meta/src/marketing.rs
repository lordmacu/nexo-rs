// Phase 82.15 — wire types for the `nexo-rs-extension-marketing`
// extension + `agent-creator` microapp. Per-field rustdoc lives
// inline; the crate-wide `deny(missing_docs)` is relaxed for
// this module while it stabilises (followup: complete docs once
// the extension lands a v0.1.0 the operator can install).
#![allow(missing_docs)]

//! Phase 82.15 — `marketing` wire types for the
//! `nexo-rs-extension-marketing` extension + `agent-creator`
//! microapp.
//!
//! Bit-equivalent across:
//!   - extension's tool handlers (Rust)
//!   - extension's HTTP admin API (Rust)
//!   - microapp's HTTP proxy (Rust)
//!   - microapp's frontend (TypeScript via `serde_typescript`)
//!
//! Multi-tenant by construction: every record carries a
//! `tenant_id` (or is implicitly tenant-scoped by per-tenant DB
//! file paths the extension uses). Caller must validate tenant
//! ownership server-side; never trust client-supplied ids.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Newtype ids ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct LeadId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PersonId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct CompanyId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct SellerId(pub String);

/// Stable kebab-case tenant id. Matches `^[a-z][a-z0-9-]{1,30}$`.
/// Mirrors `crates/tool-meta/src/admin/tenants.rs::TenantSummary::id`
/// but kept as a typed wrapper so call sites can't accidentally
/// swap a tenant id with a person id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct TenantIdRef(pub String);

// ── Enums ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LeadState {
    Cold,
    Engaged,
    MeetingScheduled,
    Qualified,
    Lost,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DomainKind {
    Personal,
    Corporate,
    Disposable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentStatus {
    None,
    SignatureParsed,
    LlmExtracted,
    CrossLinked,
    ApiEnriched,
    Manual,
    PersonalOnlyGiveup,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SentimentBand {
    VeryNegative,
    Negative,
    Neutral,
    Positive,
    VeryPositive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum IntentClass {
    Browsing,
    Comparing,
    ReadyToBuy,
    Objecting,
    SupportRequest,
    OutOfScope,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MailboxMode {
    /// Push via IMAP IDLE. Lowest latency.
    Idle,
    /// IDLE first, fall back to poll on drop, return to IDLE
    /// next reconnect.
    Adaptive,
    /// Plain `FETCH` every `poll_interval_seconds`.
    Poll,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    Pending,
    Approved,
    Rejected,
}

// ── Routing predicate AST ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RulePredicate {
    /// Exact-match domain kind from the classifier.
    SenderDomainKind {
        value: DomainKind,
    },
    /// Glob match against the full sender email
    /// (e.g. `*@acme.com`, `juan@*`).
    SenderEmailMatches {
        pattern: String,
    },
    CompanyIndustry {
        value: String,
    },
    PersonHasTag {
        tag: String,
    },
    ScoreGte {
        score: u8,
    },
    BodyContains {
        needle: String,
    },
    SubjectContains {
        needle: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssignTarget {
    Seller {
        id: SellerId,
    },
    RoundRobin {
        pool: Vec<SellerId>,
    },
    /// Drop the inbound silently — never create a lead, never
    /// notify. Used for disposable / spam routing rules.
    Drop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingRule {
    /// Stable id used for audit ("matched rule `vip-personal`").
    pub id: String,
    /// Operator-facing label.
    pub name: String,
    /// All conditions must match (AND). For OR, author multiple
    /// rules in priority order.
    pub conditions: Vec<RulePredicate>,
    pub assigns_to: AssignTarget,
    /// Reference to a `FollowupProfile.id`.
    pub followup_profile: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleSet {
    pub tenant_id: TenantIdRef,
    pub version: u32,
    pub rules: Vec<RoutingRule>,
    /// Default when no rule matches. Most operators set this to
    /// a `RoundRobin` pool of every active seller.
    pub default_target: AssignTarget,
}

// ── Followup profile ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FollowupProfile {
    pub id: String,
    /// Ordered list of delays (e.g. `["24h", "72h", "168h"]`).
    /// Parsed via `humantime::parse_duration` on the consumer
    /// side; kept as strings here so the YAML stays human-readable.
    pub cadence: Vec<String>,
    pub max_attempts: u8,
    /// When `true` (default), a client reply on the thread
    /// cancels every remaining followup.
    pub stop_on_reply: bool,
}

// ── Person / Company / Seller records ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Person {
    pub id: PersonId,
    pub tenant_id: TenantIdRef,
    pub primary_name: String,
    pub primary_email: String,
    pub alt_emails: Vec<String>,
    pub company_id: Option<CompanyId>,
    pub enrichment_status: EnrichmentStatus,
    /// 0.0..=1.0. Operator-confirmed manual entries → 1.0.
    pub enrichment_confidence: f32,
    pub tags: Vec<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Company {
    pub id: CompanyId,
    pub tenant_id: TenantIdRef,
    pub domain: String,
    pub name: String,
    pub industry: Option<String>,
    pub size_band: Option<String>,
    /// Wall-clock UTC of last successful scrape; `None` for
    /// personal domains that the scraper skipped.
    pub enriched_at_ms: Option<i64>,
    pub is_personal_domain: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingHoursWindow {
    /// IANA timezone (`America/Bogota`).
    pub timezone: String,
    /// Per-weekday window. `None` means "off" that weekday.
    pub mon_fri: Option<DayWindow>,
    pub saturday: Option<DayWindow>,
    pub sunday: Option<DayWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DayWindow {
    /// `HH:MM` in the parent's timezone.
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Seller {
    pub id: SellerId,
    pub tenant_id: TenantIdRef,
    pub name: String,
    /// Primary outbound email. The extension publishes outbound
    /// to `plugin.outbound.email.<email-instance>` — the
    /// operator wires the credential persister so SMTP creds
    /// resolve from this address.
    pub primary_email: String,
    pub alt_emails: Vec<String>,
    pub signature_text: String,
    pub working_hours: Option<WorkingHoursWindow>,
    pub on_vacation: bool,
    /// `None` when not on vacation. Inclusive ISO 8601 dates.
    pub vacation_until: Option<DateTime<Utc>>,
    /// Optional language hint that biases the LLM toward this
    /// seller's preferred outbound style.
    pub preferred_language: Option<String>,
    /// M15.35 — bound `agents.yaml.<id>`. When set, marketing
    /// reuses the agent's `ModelRef` + `system_prompt` for AI
    /// drafts / intent detection / identity resolution. The
    /// daemon's admin RPC `agents/get` is the source of truth;
    /// marketing extension never duplicates the LLM key.
    /// `None` = seller has no AI assist (manual outbound only
    /// — operator writes drafts in the UI without LLM help).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// M15.35 — per-seller model override. When `Some`, takes
    /// precedence over the agent's default `ModelRef` for
    /// every email-related LLM call. Use case: agent uses
    /// `minimax-flash` for quick WA chat but emails benefit
    /// from `claude-opus-4-7` reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<crate::admin::agents::ModelRef>,
    /// M15.38 — per-event notification toggles + target channel.
    /// `None` = seller receives no notifications about email
    /// events. When `Some`, the marketing extension publishes to
    /// `agent.email.notification.<agent_id>` for every
    /// enabled event; the agent's runtime / forwarder consumes
    /// the topic and routes per `channel` (today: WhatsApp via
    /// the agent's existing inbound binding).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_settings: Option<SellerNotificationSettings>,
    /// M15.16 — per-seller SMTP credentials. `None` means
    /// the operator hasn't registered creds yet — outbound
    /// publish refuses to fire for this seller and surfaces
    /// a clear "missing SMTP creds" error. The `password_env`
    /// field names an environment variable; the secret value
    /// itself never lands in YAML on disk (matches the
    /// extension's existing `${ENV_VAR}` posture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp_credential: Option<SmtpCredential>,
    /// M15.21 (denormalized) — bound agent's `system_prompt`, copied
    /// at seller-save time from the daemon's `agents/get` response.
    /// Lets the AI draft generator build a faithful operator
    /// IDENTITY/SOUL/USER prompt without an admin-RPC round-trip per
    /// inbound email. `None` = no agent bound (or operator-skipped).
    /// Stays in sync via the microapp's `stamp_agent_context_on_sellers`
    /// PUT handler — manual edits to the agent surface a re-save
    /// hint in the seller form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// M15.21 — denormalized provider id (e.g. `deepseek-19c7`)
    /// pulled from `agents/get`'s `model.provider`. Used by the
    /// draft generator to call `nexo/admin/llm/complete` without
    /// resolving the agent each time. Pairs with `model_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// M15.21 — denormalized model id (e.g. `deepseek-v4-flash`).
    /// `model_override` (above) takes precedence when set; this is
    /// the agent's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// M15.21 (template fallback) — handlebars draft template for
    /// the legacy `TemplateDraftGenerator`. Only consulted when
    /// `system_prompt` is empty (no agent bound) — otherwise
    /// `AgentDraftGenerator` runs LLM completion directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_template: Option<String>,
}

/// M15.16 — operator-registered outbound SMTP credentials per
/// seller. The marketing extension reads `host` / `port` /
/// `username` directly; resolves `password` at send time by
/// reading `${password_env}` from the process environment.
///
/// Storage: persisted alongside the seller row in
/// `sellers.yaml`. The framework's Phase 82.10.n credential
/// persister is for daemon-side channel registries; SMTP creds
/// for marketing-extension-owned sellers stay on the
/// extension's per-tenant config plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmtpCredential {
    /// Stable instance label for logs + the broker topic
    /// suffix (`plugin.outbound.email.<instance>`). Operator
    /// convention: `<tenant>-<seller_id>` (e.g.
    /// `acme-pedro`).
    pub instance: String,
    /// SMTP server hostname (e.g. `smtp.gmail.com`).
    pub host: String,
    /// TCP port (typically 587 for STARTTLS, 465 for
    /// implicit TLS).
    pub port: u16,
    /// SMTP authentication username (often the seller's
    /// outbound email address).
    pub username: String,
    /// Environment variable name carrying the password /
    /// app-password / OAuth refresh token. Operator sets the
    /// env in the systemd unit / docker-compose; the YAML
    /// only stores the variable NAME, never the value. Empty
    /// string is rejected at save time.
    pub password_env: String,
    /// Whether to use STARTTLS (most modern SMTP servers).
    /// Defaults to `true` so the operator can author the
    /// credential without thinking about the field.
    #[serde(default = "default_smtp_starttls")]
    pub starttls: bool,
}

fn default_smtp_starttls() -> bool {
    true
}

// ── Notification settings + event payload (M15.38) ──────────────

/// Where a notification gets forwarded. Tagged enum so JS
/// clients pattern-match on `kind`. Each non-trivial variant
/// carries its **resolved** plugin-bridge instance — the
/// frontend reads `agent.inbound_bindings` at seller-save
/// time and bakes the instance string here so the forwarder
/// (a plugin subprocess) never needs admin-RPC access to
/// route. Stale bindings (operator re-pairs WA) require a
/// seller re-save; the form surfaces the warning.
///
/// - `Disabled` — even with toggles on, the publisher skips
///   the topic frame entirely (useful for "log only" flows).
/// - `Whatsapp { instance }` — forwarder publishes to
///   `plugin.outbound.whatsapp.<instance>`. `instance` is
///   the WA bridge id (e.g. `"personal"`, `"business"`).
/// - `Email { from_instance, to }` — forwarder publishes to
///   `plugin.outbound.email.<from_instance>`. `from_instance`
///   is the email plugin instance (mailbox id) used as the
///   SMTP sender; `to` is the operator-supplied recipient.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotificationChannel {
    Disabled,
    Whatsapp { instance: String },
    Email { from_instance: String, to: String },
}

impl Default for NotificationChannel {
    /// Default discriminator is `Whatsapp { instance: "" }` —
    /// the frontend MUST resolve a non-empty instance before
    /// save lands. Tests + serialisation round-trip survive.
    fn default() -> Self {
        Self::Whatsapp {
            instance: String::new(),
        }
    }
}

/// Per-seller notification config. Granular toggles so the
/// operator opts into noisy events (transitions on every
/// inbound) vs high-signal events (new lead, draft pending).
///
/// Default values via `SellerNotificationSettings::default`
/// — useful when the operator opts in but doesn't fine-tune.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SellerNotificationSettings {
    /// Notify on cold-thread lead creation. Default: `true`.
    #[serde(default = "default_true")]
    pub on_lead_created: bool,
    /// Notify on inbound that lands on an already-tracked
    /// thread (the client replied). Default: `true` — high-
    /// signal event most operators want to see immediately.
    #[serde(default = "default_true")]
    pub on_lead_replied: bool,
    /// Notify on every state transition (cold → engaged,
    /// engaged → meeting_scheduled, …). Default: `false` —
    /// transitions fire often during active conversations
    /// and most operators prefer the firehose for real-time.
    #[serde(default)]
    pub on_lead_transitioned: bool,
    /// Notify when an AI draft awaits operator approval.
    /// Default: `true`. Wires up alongside the M22 draft
    /// pipeline; the topic is published from there.
    #[serde(default = "default_true")]
    pub on_draft_pending: bool,
    /// Notify when the meeting-intent classifier hits high
    /// confidence on an inbound (Calendly URL, "podemos vernos
    /// el martes", …). Default: `true`.
    #[serde(default = "default_true")]
    pub on_meeting_intent: bool,
    /// Target channel the forwarder uses. See
    /// [`NotificationChannel`].
    #[serde(default)]
    pub channel: NotificationChannel,
}

fn default_true() -> bool {
    true
}

impl Default for SellerNotificationSettings {
    fn default() -> Self {
        Self {
            on_lead_created: true,
            on_lead_replied: true,
            on_lead_transitioned: false,
            on_draft_pending: true,
            on_meeting_intent: true,
            channel: NotificationChannel::default(),
        }
    }
}

// ── Custom notification templates (M15.44) ──────────────────────

/// Per-tenant template overrides for notification summaries.
/// Operator writes one of these to
/// `${state_root}/marketing/<tenant_id>/notification_templates.yaml`
/// so each tenant brands its own alerts. Empty file →
/// `render_summary` falls back to the framework's hardcoded
/// ES/EN strings.
///
/// Template body uses `nexo-tool-meta::template`'s `{{path}}`
/// syntax. Available placeholders per kind:
///
/// - `{{from}}` — sender's display name (or email when no name).
/// - `{{from_email}}` — raw email address.
/// - `{{subject}}` — email subject line.
/// - `{{seller}}` — seller's name.
/// - `{{seller_email}}` — seller's primary_email.
/// - `{{lead_id}}` — uuid string.
/// - `{{state_from}}` / `{{state_to}}` — for `lead_transitioned`.
/// - `{{reason}}` — operator-supplied reason on transition.
/// - `{{confidence_pct}}` — `(confidence * 100).round()` for
///   `meeting_intent`.
/// - `{{evidence}}` — first 120 chars of the body for
///   `meeting_intent`.
///
/// Unknown placeholders render as `{{path}}` literal so
/// operators see broken templates clearly without crashing
/// the publisher.
///
/// Each kind carries an optional ES + EN template; the
/// publisher picks via `seller.preferred_language`. Missing
/// language → fall through to the other; both missing →
/// fall through to the framework's hardcoded default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NotificationTemplates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_created: Option<TemplateLocaleSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_replied: Option<TemplateLocaleSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_transitioned: Option<TemplateLocaleSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_intent: Option<TemplateLocaleSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_pending: Option<TemplateLocaleSet>,
}

/// Per-locale template strings. `es` is the operator's
/// default; `en` is the fallback for English-speaking sellers
/// (selected via `seller.preferred_language == "en"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TemplateLocaleSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub es: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
}

impl TemplateLocaleSet {
    /// Pick the template for `lang`; falls back to the other
    /// locale when missing, then to `None` if both empty.
    pub fn for_lang(&self, lang: &str) -> Option<&str> {
        match lang {
            "en" => self.en.as_deref().or(self.es.as_deref()),
            _ => self.es.as_deref().or(self.en.as_deref()),
        }
    }
}

/// Discriminated by `kind` so JS clients pattern-match on the
/// string without typed access. Mirrors `LeadFirehoseEvent`'s
/// shape but scoped to *operator-facing* notifications (vs the
/// firehose, which is UI-data-binding).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmailNotificationKind {
    LeadCreated,
    /// Inbound lands on a thread that already has a tracked
    /// lead — the client just replied. Fires from the broker
    /// hop's existing-thread branch.
    LeadReplied,
    LeadTransitioned,
    DraftPending,
    MeetingIntent,
}

/// Operator-facing notification published by the marketing
/// extension on `agent.email.notification.<agent_id>`. The
/// agent's runtime (or a sidecar) subscribes and forwards via
/// the configured channel.
///
/// The `summary` field is pre-rendered for forwarders that
/// don't want to template per-kind; the typed `kind` + payload
/// fields let smarter forwarders compose richer messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailNotification {
    /// Discriminator — `lead_created`, `lead_transitioned`,
    /// `draft_pending`, `meeting_intent`.
    pub kind: EmailNotificationKind,
    pub tenant_id: TenantIdRef,
    /// Bound `agents.yaml.<id>` — the topic suffix carries the
    /// same value so wildcard subscriptions work cleanly.
    pub agent_id: String,
    pub lead_id: LeadId,
    pub seller_id: SellerId,
    pub seller_email: String,
    /// Sender email — the lead's `from_email`.
    pub from_email: String,
    pub subject: String,
    pub at_ms: i64,
    /// Operator-facing single-paragraph summary the forwarder
    /// can use as the WA message body verbatim. Pre-localised
    /// to the seller's `preferred_language` when set.
    pub summary: String,
    /// Channel the forwarder routes to (mirrors the seller's
    /// `notification_settings.channel`). See
    /// [`NotificationChannel`].
    pub channel: NotificationChannel,
}

// ── Mailbox config ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveHoursWindow {
    pub timezone: String,
    pub mon_fri: Option<DayWindow>,
    pub saturday: Option<DayWindow>,
    pub sunday: Option<DayWindow>,
    /// Polling interval to use OUTSIDE the active window.
    /// Defaults to 5 minutes (300) so weekends / nights cost
    /// less while mailbox stays alive.
    pub off_hours_poll_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxConfig {
    pub id: String,
    pub tenant_id: TenantIdRef,
    pub address: String,
    /// Resolves to a credential persister kind in the framework
    /// (`gmail` / `outlook` / `imap_password`). Wire shape
    /// stays string so new providers don't break this enum.
    pub provider: String,
    pub mode: MailboxMode,
    pub poll_interval_seconds: u32,
    pub active: bool,
    /// `true` = operator approves drafts before send.
    /// `false` = autonomous outbound. Combined with topic
    /// guardrails (M21) so even autonomous mailboxes can route
    /// sensitive topics through the approval queue.
    pub draft_mode: bool,
    pub active_hours: Option<ActiveHoursWindow>,
    /// Email-plugin instance name this mailbox routes through.
    /// Outbound publishes to `plugin.outbound.email.<instance>`
    /// + inbound subscribes to `plugin.inbound.email.<instance>`.
    pub email_plugin_instance: String,
}

// ── Lead + thread ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lead {
    pub id: LeadId,
    pub tenant_id: TenantIdRef,
    pub thread_id: String,
    pub subject: String,
    pub person_id: PersonId,
    pub seller_id: SellerId,
    pub state: LeadState,
    /// 0..=100 heuristic score. See SDK
    /// `nexo-microapp-sdk::scoring::HeuristicScorer`.
    pub score: u8,
    pub sentiment: SentimentBand,
    pub intent: IntentClass,
    pub topic_tags: Vec<String>,
    pub last_activity_ms: i64,
    /// `None` = no followup scheduled (qualified / lost / awaiting
    /// client). `Some(ms)` = next sweep tick eligible.
    pub next_check_at_ms: Option<i64>,
    pub followup_attempts: u8,
    /// Audit trail explaining the routing decision. Operator-
    /// readable strings; surfaced in the lead context panel
    /// "why this lead?" section.
    pub why_routed: Vec<String>,
    /// M15.21.notes — free-form operator notes, edited from the
    /// lead context panel. `None` = never written; empty string
    /// = explicitly cleared. Persisted as the `operator_notes`
    /// column on the lead row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundDraft {
    pub thread_id: String,
    pub lead_id: LeadId,
    pub seller_id: SellerId,
    pub body: String,
    pub status: DraftStatus,
    pub created_at_ms: i64,
    /// Idempotency key the extension stamps on
    /// `OutboundCommand` so a double-approve doesn't double-send.
    pub idempotency_key: String,
}

// ── Tool args + responses ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeadProfileArgs {
    pub tenant_id: TenantIdRef,
    pub from_email: String,
    pub subject: String,
    /// First ~400 chars; the full body lives in the email
    /// plugin's broker payload.
    pub body_excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeadProfileResponse {
    pub person_id: PersonId,
    pub company_id: Option<CompanyId>,
    pub enrichment_status: EnrichmentStatus,
    pub enrichment_confidence: f32,
    /// `true` when the resolver merged this email into an
    /// existing person record (multi-email merge happened on
    /// this call). Microapp uses it to surface a "merged into
    /// Juan García" affordance.
    pub merged_into_existing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeadRouteArgs {
    pub tenant_id: TenantIdRef,
    pub lead_id: LeadId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeadRouteResponse {
    pub seller_id: Option<SellerId>,
    pub matched_rule_id: Option<String>,
    /// Empty when `assigns_to: drop` matched.
    pub why_routed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeetingIntent {
    pub accepted: bool,
    /// ISO 8601 with offset. `None` when the operator must still
    /// nail down the exact time.
    pub proposed_time_iso: Option<String>,
    /// 0.0..=1.0. Operator confirms above the operator-set
    /// threshold (default 0.7) before the lead state advances.
    pub confidence: f32,
    /// Quote from the email body that triggered the match —
    /// surfaced in the operator UI for one-click confirmation.
    pub evidence: String,
}

// ── Enrichment trace ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrichmentResult {
    /// Which `EnrichmentSource` produced this row
    /// (`display_name`, `signature`, `llm_extractor`, …).
    pub source: String,
    pub confidence: f32,
    pub person_inferred: Option<PersonInferred>,
    pub company_inferred: Option<CompanyInferred>,
    /// Free-form audit note ("matched signature line N").
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonInferred {
    pub name: Option<String>,
    pub role: Option<String>,
    pub seniority: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompanyInferred {
    pub name: Option<String>,
    pub domain: Option<String>,
    pub industry: Option<String>,
}

// ── Lead transition events (NATS firehose) ──────────────────────

/// Subject pattern: `agent.lead.transition.<tenant_id>.<lead_id>`.
/// Tenant-scoped so empresa A's subscriber cannot wildcard-
/// match B's lead transitions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeadTransitionEvent {
    pub tenant_id: TenantIdRef,
    pub lead_id: LeadId,
    pub from: LeadState,
    pub to: LeadState,
    pub at_ms: i64,
    pub reason: String,
    /// Optional tool call ids that triggered the transition,
    /// useful for the replay timeline UI.
    pub tool_call_ids: Vec<String>,
    /// Free-form metadata (rule id matched, draft id approved,
    /// followup attempt number, …).
    pub meta: BTreeMap<String, String>,
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, to_string};

    fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(
        value: &T,
    ) {
        let s = to_string(value).expect("serialize");
        let back: T = from_str(&s).expect("deserialize");
        assert_eq!(value, &back);
    }

    #[test]
    fn lead_state_roundtrip() {
        for s in [
            LeadState::Cold,
            LeadState::Engaged,
            LeadState::MeetingScheduled,
            LeadState::Qualified,
            LeadState::Lost,
        ] {
            roundtrip(&s);
        }
    }

    #[test]
    fn lead_state_serialises_snake_case() {
        let s = to_string(&LeadState::MeetingScheduled).unwrap();
        assert_eq!(s, "\"meeting_scheduled\"");
    }

    #[test]
    fn domain_kind_roundtrip() {
        for k in [
            DomainKind::Personal,
            DomainKind::Corporate,
            DomainKind::Disposable,
        ] {
            roundtrip(&k);
        }
    }

    #[test]
    fn rule_predicate_tagged_union() {
        let p = RulePredicate::ScoreGte { score: 70 };
        let s = to_string(&p).unwrap();
        assert!(s.contains("\"kind\":\"score_gte\""));
        roundtrip(&p);
    }

    #[test]
    fn assign_target_round_robin_roundtrip() {
        let t = AssignTarget::RoundRobin {
            pool: vec![SellerId("pedro".into()), SellerId("ana".into())],
        };
        roundtrip(&t);
    }

    #[test]
    fn routing_rule_full_roundtrip() {
        let r = RoutingRule {
            id: "vip-personal".into(),
            name: "VIP personal".into(),
            conditions: vec![RulePredicate::PersonHasTag { tag: "vip".into() }],
            assigns_to: AssignTarget::Seller {
                id: SellerId("ana".into()),
            },
            followup_profile: "vip".into(),
            active: true,
        };
        roundtrip(&r);
    }

    #[test]
    fn followup_profile_roundtrip() {
        let p = FollowupProfile {
            id: "default".into(),
            cadence: vec!["24h".into(), "72h".into(), "168h".into()],
            max_attempts: 3,
            stop_on_reply: true,
        };
        roundtrip(&p);
    }

    #[test]
    fn person_full_roundtrip() {
        let p = Person {
            id: PersonId("juan".into()),
            tenant_id: TenantIdRef("acme".into()),
            primary_name: "Juan García".into(),
            primary_email: "juan@acme.com".into(),
            alt_emails: vec!["juan.alt@gmail.com".into()],
            company_id: Some(CompanyId("acme".into())),
            enrichment_status: EnrichmentStatus::ApiEnriched,
            enrichment_confidence: 0.95,
            tags: vec!["recurring".into()],
            created_at_ms: 1_700_000_000_000,
            last_seen_at_ms: 1_700_900_000_000,
        };
        roundtrip(&p);
    }

    #[test]
    fn lead_full_roundtrip() {
        let l = Lead {
            id: LeadId("lead-001".into()),
            tenant_id: TenantIdRef("acme".into()),
            thread_id: "th-001".into(),
            subject: "Re: cotización".into(),
            person_id: PersonId("juan".into()),
            seller_id: SellerId("pedro".into()),
            state: LeadState::Engaged,
            score: 73,
            sentiment: SentimentBand::Positive,
            intent: IntentClass::ReadyToBuy,
            topic_tags: vec!["pricing".into()],
            last_activity_ms: 1_700_000_000_000,
            next_check_at_ms: Some(1_700_259_200_000),
            followup_attempts: 0,
            why_routed: vec!["score 73 >= 70".into()],
            operator_notes: None,
        };
        roundtrip(&l);
    }

    #[test]
    fn meeting_intent_roundtrip() {
        let m = MeetingIntent {
            accepted: true,
            proposed_time_iso: Some("2026-05-12T15:00:00-05:00".into()),
            confidence: 0.85,
            evidence: "yes Tuesday at 3pm".into(),
        };
        roundtrip(&m);
    }

    #[test]
    fn mailbox_config_roundtrip() {
        let m = MailboxConfig {
            id: "ventas-acme".into(),
            tenant_id: TenantIdRef("acme".into()),
            address: "ventas@acme.com".into(),
            provider: "gmail".into(),
            mode: MailboxMode::Adaptive,
            poll_interval_seconds: 60,
            active: true,
            draft_mode: true,
            active_hours: Some(ActiveHoursWindow {
                timezone: "America/Bogota".into(),
                mon_fri: Some(DayWindow {
                    start: "07:00".into(),
                    end: "20:00".into(),
                }),
                saturday: None,
                sunday: None,
                off_hours_poll_seconds: 300,
            }),
            email_plugin_instance: "acme-ventas".into(),
        };
        roundtrip(&m);
    }

    #[test]
    fn lead_transition_event_roundtrip() {
        let mut meta = BTreeMap::new();
        meta.insert("rule_id".into(), "corporate-warm".into());
        let e = LeadTransitionEvent {
            tenant_id: TenantIdRef("acme".into()),
            lead_id: LeadId("lead-001".into()),
            from: LeadState::Cold,
            to: LeadState::Engaged,
            at_ms: 1_700_000_000_000,
            reason: "rule corporate-warm matched".into(),
            tool_call_ids: vec!["call-1".into()],
            meta,
        };
        roundtrip(&e);
    }

    #[test]
    fn enrichment_result_partial_optionals() {
        let r = EnrichmentResult {
            source: "signature".into(),
            confidence: 0.78,
            person_inferred: Some(PersonInferred {
                name: Some("Juan".into()),
                role: Some("VP Sales".into()),
                seniority: Some("VP".into()),
            }),
            company_inferred: None,
            note: None,
        };
        roundtrip(&r);
    }

    #[test]
    fn seller_without_agent_binding_roundtrip() {
        // Backward compat — `agent_id` + `model_override` are
        // optional + skip-on-none, so existing YAML without
        // these fields parses cleanly.
        let v = Seller {
            id: SellerId("pedro".into()),
            tenant_id: TenantIdRef("acme".into()),
            name: "Pedro García".into(),
            primary_email: "pedro@acme.com".into(),
            alt_emails: Vec::new(),
            signature_text: "—\nPedro".into(),
            working_hours: None,
            on_vacation: false,
            vacation_until: None,
            preferred_language: None,
            agent_id: None,
            model_override: None,
            notification_settings: None,
            smtp_credential: None,
            system_prompt: None,
            model_provider: None,
            model_id: None,
            draft_template: None,
        };
        roundtrip(&v);
        // Serialised JSON should not include the optional
        // fields when None — operators see clean YAML.
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("agent_id"), "agent_id leaked: {s}");
        assert!(!s.contains("model_override"), "model_override leaked: {s}");
    }

    #[test]
    fn seller_with_agent_binding_and_override_roundtrip() {
        use crate::admin::agents::ModelRef;
        let v = Seller {
            id: SellerId("pedro".into()),
            tenant_id: TenantIdRef("acme".into()),
            name: "Pedro García".into(),
            primary_email: "pedro@acme.com".into(),
            alt_emails: Vec::new(),
            signature_text: "—\nPedro".into(),
            working_hours: None,
            on_vacation: false,
            vacation_until: None,
            preferred_language: Some("es".into()),
            agent_id: Some("ana".into()),
            model_override: Some(ModelRef {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
            }),
            notification_settings: None,
            smtp_credential: None,
            system_prompt: None,
            model_provider: None,
            model_id: None,
            draft_template: None,
        };
        roundtrip(&v);
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"agent_id\":\"ana\""), "{s}");
        assert!(s.contains("\"provider\":\"anthropic\""), "{s}");
    }

    #[test]
    fn notification_channel_whatsapp_carries_instance() {
        let ch = NotificationChannel::Whatsapp {
            instance: "personal".into(),
        };
        roundtrip(&ch);
        let s = serde_json::to_string(&ch).unwrap();
        assert!(s.contains(r#""kind":"whatsapp""#), "{s}");
        assert!(s.contains(r#""instance":"personal""#), "{s}");
    }

    #[test]
    fn notification_channel_email_carries_from_instance_and_to() {
        let ch = NotificationChannel::Email {
            from_instance: "ventas-acme".into(),
            to: "ops@acme.com".into(),
        };
        roundtrip(&ch);
        let s = serde_json::to_string(&ch).unwrap();
        assert!(s.contains(r#""kind":"email""#), "{s}");
        assert!(s.contains(r#""from_instance":"ventas-acme""#), "{s}");
        assert!(s.contains(r#""to":"ops@acme.com""#), "{s}");
    }

    #[test]
    fn notification_channel_default_is_empty_whatsapp_instance() {
        // Default exists for serde-default fallback when
        // operator YAML omits the `channel` field. Empty
        // instance means the forwarder skips silently — the
        // frontend MUST resolve before save lands.
        match NotificationChannel::default() {
            NotificationChannel::Whatsapp { instance } => {
                assert!(instance.is_empty());
            }
            other => panic!("expected default Whatsapp, got {other:?}"),
        }
    }

    #[test]
    fn seller_notification_settings_default_matches_spec() {
        let s = SellerNotificationSettings::default();
        assert!(s.on_lead_created);
        assert!(s.on_lead_replied);
        assert!(!s.on_lead_transitioned);
        assert!(s.on_draft_pending);
        assert!(s.on_meeting_intent);
        // Default is `Whatsapp { instance: "" }` — caller
        // (frontend) MUST resolve the binding before save.
        assert!(matches!(
            s.channel,
            NotificationChannel::Whatsapp { ref instance } if instance.is_empty()
        ));
    }

    #[test]
    fn seller_notification_settings_partial_payload_uses_serde_defaults() {
        // Operator writes only `channel` — the toggles default
        // via the field-level `#[serde(default = …)]` attrs.
        let json = r#"{
            "channel": {
                "kind": "email",
                "from_instance": "ventas-acme",
                "to": "ops@acme.com"
            }
        }"#;
        let parsed: SellerNotificationSettings = serde_json::from_str(json).unwrap();
        assert!(parsed.on_lead_created);
        assert!(parsed.on_lead_replied);
        assert!(!parsed.on_lead_transitioned);
        assert!(parsed.on_draft_pending);
        assert!(parsed.on_meeting_intent);
        assert_eq!(
            parsed.channel,
            NotificationChannel::Email {
                from_instance: "ventas-acme".into(),
                to: "ops@acme.com".into(),
            }
        );
    }

    #[test]
    fn notification_templates_default_is_all_none() {
        // Default = empty struct. Every locale set is None →
        // render_summary falls back to the hardcoded strings.
        let t = NotificationTemplates::default();
        assert!(t.lead_created.is_none());
        assert!(t.lead_replied.is_none());
        assert!(t.lead_transitioned.is_none());
        assert!(t.meeting_intent.is_none());
        assert!(t.draft_pending.is_none());
    }

    #[test]
    fn template_locale_set_picks_requested_lang_first() {
        let set = TemplateLocaleSet {
            es: Some("ES".into()),
            en: Some("EN".into()),
        };
        assert_eq!(set.for_lang("es"), Some("ES"));
        assert_eq!(set.for_lang("en"), Some("EN"));
    }

    #[test]
    fn template_locale_set_falls_through_to_other_lang() {
        let set = TemplateLocaleSet {
            es: Some("ES only".into()),
            en: None,
        };
        assert_eq!(set.for_lang("en"), Some("ES only"));
        assert_eq!(set.for_lang("es"), Some("ES only"));
    }

    #[test]
    fn template_locale_set_empty_both_returns_none() {
        let set = TemplateLocaleSet::default();
        assert_eq!(set.for_lang("es"), None);
        assert_eq!(set.for_lang("en"), None);
    }

    #[test]
    fn notification_templates_partial_payload_uses_serde_defaults() {
        // Operator only writes `lead_created.es` — every other
        // field defaults to None. The render_summary fallback
        // chain handles the missing variants.
        let json = r#"{
            "lead_created": {
                "es": "🚀 [Acme] Lead caliente: {{from}}\nAsunto: {{subject}}"
            }
        }"#;
        let parsed: NotificationTemplates = serde_json::from_str(json).unwrap();
        assert!(parsed.lead_created.is_some());
        assert_eq!(
            parsed.lead_created.as_ref().unwrap().es.as_deref(),
            Some("🚀 [Acme] Lead caliente: {{from}}\nAsunto: {{subject}}")
        );
        assert!(parsed.lead_replied.is_none());
    }

    #[test]
    fn email_notification_full_roundtrip() {
        let n = EmailNotification {
            kind: EmailNotificationKind::LeadCreated,
            tenant_id: TenantIdRef("acme".into()),
            agent_id: "pedro-agent".into(),
            lead_id: LeadId("l-42".into()),
            seller_id: SellerId("pedro".into()),
            seller_email: "pedro@acme.com".into(),
            from_email: "cliente@empresa.com".into(),
            subject: "Cotización".into(),
            at_ms: 1_700_000_000_000,
            summary: "📧 Nuevo lead de cliente@empresa.com (Cotización)".into(),
            channel: NotificationChannel::Whatsapp {
                instance: "personal".into(),
            },
        };
        roundtrip(&n);
    }
}
