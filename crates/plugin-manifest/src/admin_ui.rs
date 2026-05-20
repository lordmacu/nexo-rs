//! Phase 99 Stage 1 — `[plugin.admin_ui]` manifest section (Mode A).
//!
//! Lets a plugin declare admin-UI **contributions** (sidebar /
//! command-palette entries) plus **screens** (declarative config
//! forms) that the admin React shell (`nexo-rs-plugin-admin`)
//! renders generically — no per-plugin JS, no admin fork.
//!
//! ## Hybrid descriptor model
//!
//! This section is the **cold contract**: it declares *what* a
//! plugin may contribute (slots, screens, trust-gated). The
//! *live* descriptor (current field values, dynamic select
//! options) flows at runtime through the admin RPC
//! `nexo/admin/plugin_ui/describe`:
//!
//! - `describe = false` (default) — the daemon SYNTHESISES the
//!   descriptor from the `screens.fields` declared here plus the
//!   plugin's current `cfg.plugins.<id>` values.
//! - `describe = true` — the daemon FORWARDS `describe` to the
//!   plugin (via `[plugin.admin]` broker dispatch) so the plugin
//!   can vary fields / values / options by state.
//!
//! ## Mode A vs Mode B
//!
//! v1 ships **Mode A (declarative)** only. `mode = "embedded"`
//! (Mode B — iframe-mounted ESM bundle) parses but is rejected by
//! the validator with a pointer to the v2 follow-up; it is logged
//! in `FOLLOWUPS.md` (Phase 99 Mode B).
//!
//! Field-value VALIDATION reuses [`crate::config_schema`] (the
//! plugin's JSON-Schema), so this section never re-implements
//! type checking — it only declares presentation + dispatch.
//! Secret fields (`type = "secret"`) route to the generic
//! credential store (Phase 93) at write time, never to YAML.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Schema revision of `[plugin.admin_ui]`. Bumping is a
/// breaking change for the descriptor contract.
pub const CURRENT_ADMIN_UI_SCHEMA_VERSION: u32 = 1;

/// Canonical admin RPC namespace every `method` / `load` /
/// `options_source` reference must live under. Mirrors the
/// `[plugin.admin]` contract (`crate::admin`).
pub const ADMIN_RPC_PREFIX: &str = "nexo/admin/";

/// Max characters allowed in a `visible_when` expression.
/// Single-sourced from the DSL module ([`crate::visible_when`]),
/// which enforces length + grammar + AST-depth at parse time.
pub const VISIBLE_WHEN_MAX_LEN: usize = crate::visible_when::MAX_LEN;

/// Well-known core slots a contribution may target. Anything not
/// in this set must be the plugin's own namespace
/// (`plugin.<id>.<seg>`). Trust-tier gating of these slots is
/// enforced at runtime (Phase 99.4 slot registry), not here —
/// this list only validates that the slot *name* is well-formed.
pub const CORE_SLOTS: &[&str] = &[
    "core.sidebar.root",
    "core.sidebar.channels",
    "core.sidebar.integrations",
    "core.sidebar.settings",
    "core.agent_detail.tabs",
    "core.command_palette.actions",
];

/// Whitelisted `lucide-react` icon names a contribution may use.
/// Kept as a curated subset so a typo'd icon fails validation
/// instead of rendering a blank square in the rail.
pub const LUCIDE_SUBSET: &[&str] = &[
    "settings",
    "mail",
    "calendar",
    "key",
    "lock",
    "shield",
    "globe",
    "link",
    "database",
    "cloud",
    "bell",
    "user",
    "users",
    "message-circle",
    "message-square",
    "send",
    "phone",
    "bot",
    "cpu",
    "server",
    "plug",
    "puzzle",
    "sliders",
    "wrench",
    "tool",
    "activity",
    "bar-chart",
    "pie-chart",
    "list",
    "grid",
    "folder",
    "file",
    "search",
    "filter",
    "refresh-cw",
    "download",
    "upload",
    "check",
    "x",
    "alert-triangle",
    "info",
    "eye",
    "eye-off",
    "play",
    "pause",
    "trash",
    "plus",
    "edit",
    "external-link",
    "zap",
    "terminal",
    "webhook",
    "map-pin",
    "clock",
    "tag",
    "star",
    "home",
    "layout",
];

// ── Top-level section ────────────────────────────────────────────

/// `[plugin.admin_ui]` — admin UI contribution contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginAdminUiSection {
    /// Schema revision. Must equal [`CURRENT_ADMIN_UI_SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Render mode. v1 accepts only `declarative`; `embedded`
    /// (Mode B) is rejected by the validator pending v2.
    #[serde(default)]
    pub mode: AdminUiMode,

    /// When `true`, the plugin serves the live descriptor via
    /// `<method_prefix>admin_ui/describe`. When `false`, the
    /// daemon synthesises it from `screens.fields` + current
    /// config — so each screen MUST declare its fields.
    #[serde(default)]
    pub describe: bool,

    /// Menu / palette entries. Each points at a `screen` (except
    /// pure command-palette actions, which may omit it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<Contribution>,

    /// Declarative screens referenced by `contributions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<Screen>,
}

fn default_schema_version() -> u32 {
    CURRENT_ADMIN_UI_SCHEMA_VERSION
}

/// Render mode discriminator.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdminUiMode {
    /// Daemon-rendered generic form (Mode A). The only mode in v1.
    #[default]
    Declarative,
    /// Iframe-mounted ESM bundle (Mode B). Parsed but rejected in
    /// v1 — see `FOLLOWUPS.md` Phase 99 Mode B.
    Embedded,
}

/// A label that is either a plain string or a BCP-47 → string map
/// (Phase 89 locale-aware UI). Untagged so TOML authors write
/// `label = "Google"` or `label = { en = "Google", es = "Google" }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum I18nLabel {
    Plain(String),
    Localized(BTreeMap<String, String>),
}

impl I18nLabel {
    /// `true` when the label carries no renderable text.
    pub fn is_empty(&self) -> bool {
        match self {
            I18nLabel::Plain(s) => s.trim().is_empty(),
            I18nLabel::Localized(m) => m.is_empty() || m.values().all(|v| v.trim().is_empty()),
        }
    }
}

// ── Contributions ────────────────────────────────────────────────

/// A single menu / sidebar / command-palette entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    /// Kebab-case id, unique within the plugin's section.
    pub id: String,

    /// Target slot for a TOP-LEVEL entry (menu): a [`CORE_SLOTS`]
    /// entry or `plugin.<id>.<seg>`. Omit when `parent` is set —
    /// the entry then nests as a submenu under its parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,

    /// Parent contribution id. When set, this entry renders as a
    /// SUBMENU item under that parent (menu → submenu nesting),
    /// so a freshly installed plugin can extend an existing menu
    /// instead of only creating a new top-level one. Mutually
    /// exclusive with `slot` (parent wins; slot ignored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Rail / menu label.
    pub label: I18nLabel,

    /// Optional `lucide-react` icon name ([`LUCIDE_SUBSET`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Sort key. Plugin entries conventionally use 1000+.
    #[serde(default)]
    pub order: u32,

    /// Screen this entry opens. `None` is legal only for
    /// command-palette actions that fire an RPC without a form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,

    /// Optional `visible_when` expression (Phase 99.2 DSL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<String>,
}

// ── Screens ──────────────────────────────────────────────────────

/// A declarative config screen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Screen {
    /// Kebab-case id, unique within the section. Referenced by
    /// `Contribution.screen`.
    pub id: String,

    /// Screen heading.
    pub title: I18nLabel,

    /// Optional RPC that hydrates field values on open. Must live
    /// under [`ADMIN_RPC_PREFIX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<String>,

    /// Form fields. Required when the section's `describe = false`
    /// (daemon synthesises the descriptor from these).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,

    /// Buttons. An action with `id = "save"` overrides the
    /// implicit save handler.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,

    /// Optional read-only live widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<RefreshSpec>,
}

// ── Fields ───────────────────────────────────────────────────────

/// A single form field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// Config key — maps to a `config_schema` property.
    pub key: String,

    /// Renderer kind. Serialised as `type` in TOML.
    #[serde(rename = "type")]
    pub field_type: FieldType,

    /// Field label.
    pub label: I18nLabel,

    #[serde(default)]
    pub required: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<I18nLabel>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<String>,

    /// Inline static options (select / multiselect).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOption>>,

    /// Dynamic option source (select / multiselect / list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_source: Option<OptionsSource>,
}

/// Field renderer kinds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Number,
    /// Write-only; routes to the credential store (Phase 93).
    Secret,
    Toggle,
    Select,
    Multiselect,
    List,
    Link,
    Textarea,
    Json,
}

impl FieldType {
    /// `true` for kinds whose options come from `options` /
    /// `options_source` (i.e. need a choice set).
    fn needs_options(&self) -> bool {
        matches!(self, FieldType::Select | FieldType::Multiselect)
    }
}

/// One static select option.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    pub value: String,
    pub label: I18nLabel,
}

/// How a field's choice set is sourced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OptionsSource {
    /// Use the inline `options` array.
    Static,
    /// Resolve at descriptor-build time via this admin RPC.
    Rpc { method: String },
}

// ── Actions ──────────────────────────────────────────────────────

/// A screen button that dispatches an admin RPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Kebab-case id. `"save"` overrides the implicit save.
    pub id: String,

    pub label: I18nLabel,

    /// Admin RPC method. Must live under [`ADMIN_RPC_PREFIX`].
    pub method: String,

    /// Optional confirmation copy shown before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<I18nLabel>,

    /// Optional inputs collected in a mini-form before dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_fields: Vec<Field>,

    /// How the result is rendered.
    #[serde(default)]
    pub on_success: OnSuccess,
}

/// Result-rendering mode for an action.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnSuccess {
    #[default]
    Toast,
    InlineJson,
    Table,
    Redirect,
    Refresh,
}

/// A read-only live widget that polls an admin RPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RefreshSpec {
    /// Admin RPC returning the widget payload. Must live under
    /// [`ADMIN_RPC_PREFIX`].
    pub method: String,

    /// Optional auto-poll interval. `None` = manual refresh only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
}

// ── Validation ───────────────────────────────────────────────────

impl PluginAdminUiSection {
    /// Collect every contract violation (empty `Vec` = valid).
    /// Never bails on first error — matches the manifest's
    /// "report all in one pass" policy. `plugin_id` is needed to
    /// validate `plugin.<id>.*` slot ownership.
    pub fn validate(&self, plugin_id: &str) -> Vec<String> {
        let mut e = Vec::new();

        if self.schema_version != CURRENT_ADMIN_UI_SCHEMA_VERSION {
            e.push(format!(
                "schema_version {} unsupported; v1 accepts only {}",
                self.schema_version, CURRENT_ADMIN_UI_SCHEMA_VERSION
            ));
        }

        if self.mode == AdminUiMode::Embedded {
            e.push(
                "mode \"embedded\" (Mode B) is deferred to v2; use mode \"declarative\". \
                 See FOLLOWUPS.md Phase 99 Mode B."
                    .to_string(),
            );
        }

        // Screen ids first so contribution `screen` refs can be
        // checked against the full set.
        let mut screen_ids: BTreeSet<&str> = BTreeSet::new();
        for s in &self.screens {
            if !is_kebab_id(&s.id) {
                e.push(format!(
                    "screen id `{}` must be kebab-case (a-z, 0-9, -; 1-41 chars)",
                    s.id
                ));
            }
            if !screen_ids.insert(s.id.as_str()) {
                e.push(format!("duplicate screen id `{}`", s.id));
            }
            s.validate(self.describe, &mut e);
        }

        // Pre-collect contribution ids + parent links so submenu
        // `parent` refs resolve against the full set regardless of
        // declaration order.
        let all_contrib_ids: BTreeSet<&str> =
            self.contributions.iter().map(|c| c.id.as_str()).collect();
        let parent_of: BTreeMap<&str, Option<&str>> = self
            .contributions
            .iter()
            .map(|c| (c.id.as_str(), c.parent.as_deref()))
            .collect();

        let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
        for c in &self.contributions {
            if !is_kebab_id(&c.id) {
                e.push(format!(
                    "contribution id `{}` must be kebab-case (a-z, 0-9, -; 1-41 chars)",
                    c.id
                ));
            }
            if !seen_ids.insert(c.id.as_str()) {
                e.push(format!("duplicate contribution id `{}`", c.id));
            }
            if c.label.is_empty() {
                e.push(format!("contribution `{}` label is empty", c.id));
            }
            // Placement: submenu (parent) XOR top-level (slot).
            match (&c.parent, &c.slot) {
                (Some(parent), _) => {
                    if parent.as_str() == c.id {
                        e.push(format!("contribution `{}` cannot be its own parent", c.id));
                    } else if !all_contrib_ids.contains(parent.as_str()) {
                        e.push(format!(
                            "contribution `{}` parent `{}` does not exist",
                            c.id, parent
                        ));
                    } else if let Some(at) = detect_parent_cycle(c.id.as_str(), &parent_of) {
                        e.push(format!(
                            "contribution `{}` parent chain cycles / nests too deep at `{}`",
                            c.id, at
                        ));
                    }
                }
                (None, Some(slot)) => {
                    if !slot_is_valid(slot, plugin_id) {
                        e.push(format!(
                            "contribution `{}` slot `{}` unknown; use a core slot {:?} or `plugin.{}.<seg>`",
                            c.id, slot, CORE_SLOTS, plugin_id
                        ));
                    }
                }
                (None, None) => {
                    e.push(format!(
                        "contribution `{}` needs a `slot` (top-level menu) or a `parent` (submenu)",
                        c.id
                    ));
                }
            }
            if let Some(icon) = &c.icon {
                if !LUCIDE_SUBSET.contains(&icon.as_str()) {
                    e.push(format!(
                        "contribution `{}` icon `{}` not in the lucide subset",
                        c.id, icon
                    ));
                }
            }
            if let Some(screen) = &c.screen {
                if !screen_ids.contains(screen.as_str()) {
                    e.push(format!(
                        "contribution `{}` references unknown screen `{}`",
                        c.id, screen
                    ));
                }
            }
            check_visible_when(
                c.visible_when.as_deref(),
                &format!("contribution `{}`", c.id),
                &mut e,
            );
        }

        e
    }
}

impl Screen {
    fn validate(&self, describe: bool, e: &mut Vec<String>) {
        if self.title.is_empty() {
            e.push(format!("screen `{}` title is empty", self.id));
        }
        if let Some(load) = &self.load {
            require_admin_prefix(load, &format!("screen `{}` load", self.id), e);
        }
        // describe=false ⇒ daemon synthesises the descriptor from
        // these fields, so they cannot be empty.
        if !describe && self.fields.is_empty() {
            e.push(format!(
                "screen `{}` declares no fields but `describe = false`; \
                 add fields or set `describe = true`",
                self.id
            ));
        }

        let mut field_keys: BTreeSet<&str> = BTreeSet::new();
        for f in &self.fields {
            if !field_keys.insert(f.key.as_str()) {
                e.push(format!(
                    "screen `{}` duplicate field key `{}`",
                    self.id, f.key
                ));
            }
            f.validate(&format!("screen `{}` field `{}`", self.id, f.key), e);
        }

        let mut action_ids: BTreeSet<&str> = BTreeSet::new();
        for a in &self.actions {
            if !is_kebab_id(&a.id) {
                e.push(format!(
                    "screen `{}` action id `{}` must be kebab-case",
                    self.id, a.id
                ));
            }
            if !action_ids.insert(a.id.as_str()) {
                e.push(format!(
                    "screen `{}` duplicate action id `{}`",
                    self.id, a.id
                ));
            }
            a.validate(&format!("screen `{}` action `{}`", self.id, a.id), e);
        }

        if let Some(r) = &self.refresh {
            require_admin_prefix(&r.method, &format!("screen `{}` refresh", self.id), e);
        }
    }
}

impl Field {
    fn validate(&self, ctx: &str, e: &mut Vec<String>) {
        if !is_config_key(&self.key) {
            e.push(format!(
                "{ctx}: key `{}` invalid (non-empty, ≤64, [A-Za-z0-9_.-])",
                self.key
            ));
        }
        if self.label.is_empty() {
            e.push(format!("{ctx}: label is empty"));
        }
        if self.field_type.needs_options()
            && self.options.is_none()
            && self.options_source.is_none()
        {
            e.push(format!(
                "{ctx}: select/multiselect needs `options` or `options_source`"
            ));
        }
        if let Some(OptionsSource::Rpc { method }) = &self.options_source {
            require_admin_prefix(method, &format!("{ctx} options_source"), e);
        }
        check_visible_when(self.visible_when.as_deref(), ctx, e);
    }
}

impl Action {
    fn validate(&self, ctx: &str, e: &mut Vec<String>) {
        if self.label.is_empty() {
            e.push(format!("{ctx}: label is empty"));
        }
        require_admin_prefix(&self.method, ctx, e);
        for f in &self.prompt_fields {
            f.validate(&format!("{ctx} prompt_field `{}`", f.key), e);
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// `true` for a core slot or a well-formed `plugin.<id>.<seg>`
/// owned by `plugin_id`.
fn slot_is_valid(slot: &str, plugin_id: &str) -> bool {
    if CORE_SLOTS.contains(&slot) {
        return true;
    }
    let prefix = format!("plugin.{plugin_id}.");
    if let Some(tail) = slot.strip_prefix(&prefix) {
        return is_kebab_id(tail);
    }
    false
}

/// Walk the parent chain from `start`; return the id where a
/// cycle closes (or where it exceeds the nesting cap), or `None`
/// when the chain terminates cleanly. Caps depth so a
/// pathological-but-acyclic chain can't nest forever.
fn detect_parent_cycle(start: &str, parent_of: &BTreeMap<&str, Option<&str>>) -> Option<String> {
    const MAX_DEPTH: usize = 5;
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut cur = start;
    for _ in 0..=MAX_DEPTH {
        if !visited.insert(cur) {
            return Some(cur.to_string());
        }
        match parent_of.get(cur).copied().flatten() {
            Some(p) => cur = p,
            None => return None,
        }
    }
    Some(cur.to_string())
}

/// Push an error if `method` does not live under
/// [`ADMIN_RPC_PREFIX`].
fn require_admin_prefix(method: &str, ctx: &str, e: &mut Vec<String>) {
    if !method.starts_with(ADMIN_RPC_PREFIX) {
        e.push(format!(
            "{ctx}: method `{method}` must start with `{ADMIN_RPC_PREFIX}`"
        ));
    }
}

/// `visible_when` check — full parse via the Phase 99.2 DSL
/// ([`crate::visible_when::parse`]): grammar + length + AST-depth.
fn check_visible_when(expr: Option<&str>, ctx: &str, e: &mut Vec<String>) {
    if let Some(expr) = expr {
        if let Err(err) = crate::visible_when::parse(expr) {
            e.push(format!("{ctx}: visible_when invalid: {err}"));
        }
    }
}

/// Kebab-case id: starts a-z, then a-z0-9-, length 1..=41.
fn is_kebab_id(s: &str) -> bool {
    let len = s.len();
    if !(1..=41).contains(&len) {
        return false;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Config key: non-empty, ≤64, `[A-Za-z0-9_.-]`, starts alnum.
fn is_config_key(s: &str) -> bool {
    let len = s.len();
    if !(1..=64).contains(&len) {
        return false;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const PID: &str = "google";

    fn parse(toml_body: &str) -> PluginAdminUiSection {
        toml::from_str(toml_body).expect("parse admin_ui toml")
    }

    fn minimal_static() -> PluginAdminUiSection {
        parse(
            r#"
            schema_version = 1
            mode = "declarative"
            describe = false

            [[contributions]]
            id = "google-settings"
            slot = "core.sidebar.integrations"
            label = "Google"
            icon = "mail"
            order = 1000
            screen = "smtp"

            [[screens]]
            id = "smtp"
            title = "SMTP"

            [[screens.fields]]
            key = "host"
            type = "text"
            label = "SMTP Host"
            required = true
            "#,
        )
    }

    #[test]
    fn minimal_static_is_valid() {
        assert!(minimal_static().validate(PID).is_empty());
    }

    #[test]
    fn describe_true_without_fields_is_valid() {
        let s = parse(
            r#"
            describe = true
            [[contributions]]
            id = "g"
            slot = "plugin.google.root"
            label = "G"
            screen = "main"
            [[screens]]
            id = "main"
            title = "Main"
            "#,
        );
        assert!(s.validate(PID).is_empty(), "{:?}", s.validate(PID));
    }

    #[test]
    fn embedded_mode_parses_but_rejected_with_v2_hint() {
        let s = parse(
            r#"
            mode = "embedded"
            describe = true
            [[screens]]
            id = "x"
            title = "X"
            "#,
        );
        let errs = s.validate(PID);
        assert!(errs
            .iter()
            .any(|m| m.contains("embedded") && m.contains("v2")));
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let s = parse("schema_version = 2\ndescribe = true\n");
        assert!(s.validate(PID).iter().any(|m| m.contains("schema_version")));
    }

    #[test]
    fn icon_outside_lucide_subset_rejected() {
        let mut s = minimal_static();
        s.contributions[0].icon = Some("not-a-real-icon".into());
        assert!(s.validate(PID).iter().any(|m| m.contains("lucide")));
    }

    #[test]
    fn valid_lucide_icon_accepted() {
        let mut s = minimal_static();
        s.contributions[0].icon = Some("calendar".into());
        assert!(s.validate(PID).is_empty());
    }

    #[test]
    fn unknown_slot_rejected() {
        let mut s = minimal_static();
        s.contributions[0].slot = Some("core.sidebar.bogus".into());
        assert!(s.validate(PID).iter().any(|m| m.contains("slot")));
    }

    #[test]
    fn core_slot_accepted() {
        let mut s = minimal_static();
        s.contributions[0].slot = Some("core.command_palette.actions".into());
        assert!(s.validate(PID).is_empty());
    }

    #[test]
    fn plugin_namespaced_slot_accepted() {
        let mut s = minimal_static();
        s.contributions[0].slot = Some("plugin.google.root".into());
        assert!(s.validate(PID).is_empty());
    }

    #[test]
    fn plugin_slot_for_wrong_id_rejected() {
        let mut s = minimal_static();
        s.contributions[0].slot = Some("plugin.telegram.root".into());
        assert!(s.validate(PID).iter().any(|m| m.contains("slot")));
    }

    #[test]
    fn contribution_referencing_unknown_screen_rejected() {
        let mut s = minimal_static();
        s.contributions[0].screen = Some("ghost".into());
        assert!(s.validate(PID).iter().any(|m| m.contains("unknown screen")));
    }

    #[test]
    fn action_method_without_prefix_rejected() {
        let s = parse(
            r#"
            describe = true
            [[screens]]
            id = "x"
            title = "X"
            [[screens.actions]]
            id = "send"
            label = "Send"
            method = "whatsapp/send"
            "#,
        );
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("must start with")));
    }

    #[test]
    fn action_method_with_prefix_accepted() {
        let s = parse(
            r#"
            describe = true
            [[screens]]
            id = "x"
            title = "X"
            [[screens.actions]]
            id = "send"
            label = "Send"
            method = "nexo/admin/google/smtp_test"
            "#,
        );
        assert!(s.validate(PID).is_empty());
    }

    #[test]
    fn options_source_rpc_without_prefix_rejected() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            [[screens.fields]]
            key = "cal"
            type = "select"
            label = "Calendar"
            [screens.fields.options_source]
            kind = "rpc"
            method = "google/calendars"
            "#,
        );
        assert!(s.validate(PID).iter().any(|m| m.contains("options_source")));
    }

    #[test]
    fn load_without_prefix_rejected() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            load = "google/load"
            [[screens.fields]]
            key = "host"
            type = "text"
            label = "Host"
            "#,
        );
        assert!(s.validate(PID).iter().any(|m| m.contains("load")));
    }

    #[test]
    fn describe_false_empty_fields_rejected() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            "#,
        );
        assert!(s.validate(PID).iter().any(|m| m.contains("no fields")));
    }

    #[test]
    fn duplicate_contribution_ids_rejected() {
        let s = parse(
            r#"
            describe = true
            [[contributions]]
            id = "dup"
            slot = "core.sidebar.root"
            label = "A"
            screen = "x"
            [[contributions]]
            id = "dup"
            slot = "core.command_palette.actions"
            label = "B"
            screen = "x"
            [[screens]]
            id = "x"
            title = "X"
            "#,
        );
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("duplicate contribution")));
    }

    #[test]
    fn duplicate_screen_ids_rejected() {
        let s = parse(
            r#"
            describe = true
            [[screens]]
            id = "x"
            title = "X"
            [[screens]]
            id = "x"
            title = "Y"
            "#,
        );
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("duplicate screen")));
    }

    #[test]
    fn duplicate_field_keys_rejected() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            [[screens.fields]]
            key = "host"
            type = "text"
            label = "Host"
            [[screens.fields]]
            key = "host"
            type = "text"
            label = "Host 2"
            "#,
        );
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("duplicate field key")));
    }

    #[test]
    fn duplicate_action_ids_rejected() {
        let s = parse(
            r#"
            describe = true
            [[screens]]
            id = "x"
            title = "X"
            [[screens.actions]]
            id = "go"
            label = "Go"
            method = "nexo/admin/x/a"
            [[screens.actions]]
            id = "go"
            label = "Go2"
            method = "nexo/admin/x/b"
            "#,
        );
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("duplicate action")));
    }

    #[test]
    fn select_without_options_rejected() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            [[screens.fields]]
            key = "region"
            type = "select"
            label = "Region"
            "#,
        );
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("needs `options`")));
    }

    #[test]
    fn select_with_static_options_accepted() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            [[screens.fields]]
            key = "region"
            type = "select"
            label = "Region"
            [[screens.fields.options]]
            value = "bogota"
            label = "Bogotá"
            "#,
        );
        assert!(s.validate(PID).is_empty(), "{:?}", s.validate(PID));
    }

    #[test]
    fn select_with_rpc_options_source_accepted() {
        let s = parse(
            r#"
            describe = false
            [[screens]]
            id = "x"
            title = "X"
            [[screens.fields]]
            key = "cal"
            type = "select"
            label = "Calendar"
            [screens.fields.options_source]
            kind = "rpc"
            method = "nexo/admin/google/calendars"
            "#,
        );
        assert!(s.validate(PID).is_empty(), "{:?}", s.validate(PID));
    }

    #[test]
    fn visible_when_too_long_rejected() {
        let mut s = minimal_static();
        s.contributions[0].visible_when = Some("a".repeat(VISIBLE_WHEN_MAX_LEN + 1));
        assert!(s.validate(PID).iter().any(|m| m.contains("visible_when")));
    }

    #[test]
    fn visible_when_within_limit_accepted() {
        let mut s = minimal_static();
        s.contributions[0].visible_when = Some("plugin.enabled && plugin.healthy".into());
        assert!(s.validate(PID).is_empty());
    }

    #[test]
    fn i18n_label_plain_parses() {
        let s = minimal_static();
        assert_eq!(s.contributions[0].label, I18nLabel::Plain("Google".into()));
    }

    #[test]
    fn i18n_label_localized_parses() {
        let s = parse(
            r#"
            describe = true
            [[contributions]]
            id = "g"
            slot = "core.sidebar.root"
            label = { en = "Google", es = "Google" }
            screen = "x"
            [[screens]]
            id = "x"
            title = "X"
            "#,
        );
        match &s.contributions[0].label {
            I18nLabel::Localized(m) => {
                assert_eq!(m.get("en").map(String::as_str), Some("Google"));
                assert_eq!(m.get("es").map(String::as_str), Some("Google"));
            }
            other => panic!("expected localized, got {other:?}"),
        }
    }

    #[test]
    fn submenu_parent_accepted() {
        // A plugin installs a top-level "Google" menu and nests
        // SMTP / OAuth as submenu items under it.
        let s = parse(
            r#"
            describe = true
            [[contributions]]
            id = "google"
            slot = "core.sidebar.integrations"
            label = "Google"
            [[contributions]]
            id = "smtp"
            parent = "google"
            label = "SMTP"
            screen = "smtp"
            [[contributions]]
            id = "oauth"
            parent = "google"
            label = "OAuth"
            screen = "oauth"
            [[screens]]
            id = "smtp"
            title = "SMTP"
            [[screens]]
            id = "oauth"
            title = "OAuth"
            "#,
        );
        assert!(s.validate(PID).is_empty(), "{:?}", s.validate(PID));
    }

    #[test]
    fn contribution_without_slot_or_parent_rejected() {
        let mut s = minimal_static();
        s.contributions[0].slot = None;
        s.contributions[0].parent = None;
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("needs a `slot`") && m.contains("parent")));
    }

    #[test]
    fn submenu_parent_nonexistent_rejected() {
        let mut s = minimal_static();
        s.contributions[0].slot = None;
        s.contributions[0].parent = Some("ghost-menu".into());
        assert!(s
            .validate(PID)
            .iter()
            .any(|m| m.contains("parent") && m.contains("does not exist")));
    }

    #[test]
    fn submenu_parent_cycle_rejected() {
        let s = parse(
            r#"
            describe = true
            [[contributions]]
            id = "a"
            parent = "b"
            label = "A"
            [[contributions]]
            id = "b"
            parent = "a"
            label = "B"
            "#,
        );
        assert!(s.validate(PID).iter().any(|m| m.contains("cycle")));
    }

    #[test]
    fn contribution_id_not_kebab_rejected() {
        let mut s = minimal_static();
        s.contributions[0].id = "Bad_Id".into();
        assert!(s.validate(PID).iter().any(|m| m.contains("kebab")));
    }

    #[test]
    fn empty_field_key_rejected() {
        let mut s = minimal_static();
        s.screens[0].fields[0].key = String::new();
        assert!(s.validate(PID).iter().any(|m| m.contains("key")));
    }

    #[test]
    fn full_section_serde_round_trip() {
        let s = minimal_static();
        let toml = toml::to_string(&s).expect("serialize");
        let back: PluginAdminUiSection = toml::from_str(&toml).expect("deserialize");
        assert_eq!(s, back);
    }

    #[test]
    fn on_success_defaults_to_toast() {
        let s = parse(
            r#"
            describe = true
            [[screens]]
            id = "x"
            title = "X"
            [[screens.actions]]
            id = "go"
            label = "Go"
            method = "nexo/admin/x/a"
            "#,
        );
        assert_eq!(s.screens[0].actions[0].on_success, OnSuccess::Toast);
    }

    #[test]
    fn all_field_types_deserialize() {
        for t in [
            "text",
            "number",
            "secret",
            "toggle",
            "select",
            "multiselect",
            "list",
            "link",
            "textarea",
            "json",
        ] {
            let body = format!(
                "describe = true\n[[screens]]\nid=\"x\"\ntitle=\"X\"\n\
                 [[screens.fields]]\nkey=\"k\"\ntype=\"{t}\"\nlabel=\"L\"\n\
                 [[screens.fields.options]]\nvalue=\"v\"\nlabel=\"Lv\"\n"
            );
            let s = parse(&body);
            // describe=true so empty-fields rule doesn't fire; only
            // assert it parses + (for select/multiselect) options OK.
            let _ = s.validate(PID);
        }
    }

    #[test]
    fn multiple_errors_collected_one_pass() {
        let s = parse(
            r#"
            schema_version = 9
            mode = "embedded"
            describe = false
            [[contributions]]
            id = "Bad"
            slot = "core.sidebar.bogus"
            label = "X"
            screen = "ghost"
            [[screens]]
            id = "empty"
            title = "E"
            "#,
        );
        let errs = s.validate(PID);
        assert!(errs.len() >= 5, "expected ≥5 errors, got {errs:?}");
    }
}
