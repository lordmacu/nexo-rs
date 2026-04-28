//! Phase 12.6 — `mcp_server.yaml` schema.
//!
//! Opt-in feature: expose this agent as an MCP server so Claude Desktop /
//! Cursor / Zed can invoke its tools over stdio.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfigFile {
    pub mcp_server: McpServerConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Advertised as `serverInfo.name` during MCP `initialize`. Defaults to
    /// `"agent"` when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// Explicit tool allowlist. Empty = expose all non-proxy tools; proxy
    /// tools (`ext_*`, `mcp_*`) still require `expose_proxies: true`.
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// When false (default), tool proxies generated from other runtimes
    /// (`ext_*`, `mcp_*`) are hidden unless explicitly allowlisted.
    #[serde(default)]
    pub expose_proxies: bool,
    /// Optional env var name containing the expected initialize token.
    /// When set, clients must include that token in initialize params
    /// (`auth_token` or `_meta.auth_token`).
    #[serde(default)]
    pub auth_token_env: Option<String>,
    /// Phase 76.1 — optional HTTP+SSE transport. Stdio is unaffected.
    #[serde(default)]
    pub http: Option<HttpTransportConfigYaml>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTransportConfigYaml {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_http_bind")]
    pub bind: std::net::SocketAddr,
    /// Env var holding the bearer token. `None` = no auth (only
    /// safe on loopback; the runtime refuses non-loopback bind
    /// without a token).
    #[serde(default)]
    pub auth_token_env: Option<String>,
    #[serde(default = "default_http_allow_origins")]
    pub allow_origins: Vec<String>,
    #[serde(default = "default_http_body_max_bytes")]
    pub body_max_bytes: usize,
    #[serde(default = "default_http_max_in_flight")]
    pub max_in_flight: usize,
    #[serde(default)]
    pub per_ip_rate_limit: PerIpRateLimitYaml,
    #[serde(default = "default_http_request_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_http_idle")]
    pub session_idle_timeout_secs: u64,
    #[serde(default = "default_http_lifetime")]
    pub session_max_lifetime_secs: u64,
    #[serde(default = "default_http_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_http_keepalive")]
    pub sse_keepalive_secs: u64,
    #[serde(default = "default_http_max_age")]
    pub sse_max_age_secs: u64,
    #[serde(default = "default_http_buffer")]
    pub sse_buffer_size: usize,
    #[serde(default)]
    pub enable_legacy_sse: bool,
    /// Phase 76.3 — pluggable authentication. Mutually exclusive with
    /// `auth_token_env` (legacy field). When both are set the loader
    /// fails fast.
    #[serde(default)]
    pub auth: Option<AuthConfigYaml>,
    /// Phase 76.5 — per-(tenant, tool) token-bucket rate-limit.
    /// `None` (i.e. block omitted) disables the limiter entirely;
    /// `enabled: true` (default when block present) turns it on.
    #[serde(default)]
    pub per_principal_rate_limit: Option<PerPrincipalRateLimitYaml>,
    /// Phase 76.6 — per-(tenant, tool) in-flight concurrency cap +
    /// per-call timeout. `None` (block omitted) disables.
    #[serde(default)]
    pub per_principal_concurrency: Option<PerPrincipalConcurrencyYaml>,
    /// Phase 76.11 — durable per-call audit log. `None` (block
    /// omitted) disables; otherwise the runtime opens a
    /// `SqliteAuditLogStore(db_path)` and wires it into the
    /// dispatcher.
    #[serde(default)]
    pub audit_log: Option<AuditLogYaml>,
    /// Phase 76.8 — durable session event store for SSE
    /// `Last-Event-ID` reconnect. `None` keeps the in-memory
    /// behavior; `Some(_)` with `enabled: true` opens a
    /// `SqliteSessionEventStore(db_path)`.
    #[serde(default)]
    pub session_event_store: Option<SessionEventStoreYaml>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionEventStoreYaml {
    #[serde(default = "default_ses_enabled")]
    pub enabled: bool,
    #[serde(default = "default_ses_db_path")]
    pub db_path: std::path::PathBuf,
    #[serde(default = "default_ses_max_per_session")]
    pub max_events_per_session: u64,
    #[serde(default = "default_ses_max_replay_batch")]
    pub max_replay_batch: usize,
    #[serde(default = "default_ses_purge_interval_secs")]
    pub purge_interval_secs: u64,
}

fn default_ses_enabled() -> bool {
    true
}
fn default_ses_db_path() -> std::path::PathBuf {
    std::path::PathBuf::from("data/mcp_sessions.db")
}
fn default_ses_max_per_session() -> u64 {
    10_000
}
fn default_ses_max_replay_batch() -> usize {
    1_000
}
fn default_ses_purge_interval_secs() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuditLogYaml {
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    #[serde(default = "default_audit_db_path")]
    pub db_path: std::path::PathBuf,
    #[serde(default = "default_audit_retention_secs")]
    pub retention_secs: u64,
    #[serde(default = "default_audit_writer_buffer")]
    pub writer_buffer: usize,
    #[serde(default = "default_audit_flush_interval_ms")]
    pub flush_interval_ms: u64,
    #[serde(default = "default_audit_flush_batch_size")]
    pub flush_batch_size: usize,
    #[serde(default = "default_audit_redact_args")]
    pub redact_args: bool,
    #[serde(default)]
    pub per_tool_redact_args: std::collections::BTreeMap<String, bool>,
    #[serde(default = "default_audit_args_hash_max_bytes")]
    pub args_hash_max_bytes: usize,
}

fn default_audit_enabled() -> bool {
    true
}
fn default_audit_db_path() -> std::path::PathBuf {
    std::path::PathBuf::from("data/mcp_audit.db")
}
fn default_audit_retention_secs() -> u64 {
    90 * 86_400
}
fn default_audit_writer_buffer() -> usize {
    4096
}
fn default_audit_flush_interval_ms() -> u64 {
    50
}
fn default_audit_flush_batch_size() -> usize {
    50
}
fn default_audit_redact_args() -> bool {
    true
}
fn default_audit_args_hash_max_bytes() -> usize {
    1024 * 1024
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerToolConcurrencyYaml {
    pub max_in_flight: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerPrincipalConcurrencyYaml {
    #[serde(default = "default_pc_enabled")]
    pub enabled: bool,
    #[serde(default = "default_pc_default_limit")]
    pub default: PerToolConcurrencyYaml,
    #[serde(default)]
    pub per_tool: std::collections::BTreeMap<String, PerToolConcurrencyYaml>,
    #[serde(default = "default_pc_default_timeout_secs")]
    pub default_timeout_secs: u64,
    #[serde(default = "default_pc_queue_wait_ms")]
    pub queue_wait_ms: u64,
    #[serde(default = "default_pc_max_buckets")]
    pub max_buckets: usize,
    #[serde(default = "default_pc_stale_ttl_secs")]
    pub stale_ttl_secs: u64,
}

fn default_pc_enabled() -> bool {
    true
}
fn default_pc_default_limit() -> PerToolConcurrencyYaml {
    PerToolConcurrencyYaml {
        max_in_flight: 10,
        timeout_secs: None,
    }
}
fn default_pc_default_timeout_secs() -> u64 {
    30
}
fn default_pc_queue_wait_ms() -> u64 {
    5_000
}
fn default_pc_max_buckets() -> usize {
    50_000
}
fn default_pc_stale_ttl_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerToolLimitYaml {
    pub rps: f64,
    pub burst: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerPrincipalRateLimitYaml {
    #[serde(default = "default_pp_enabled")]
    pub enabled: bool,
    #[serde(default = "default_pp_default_limit")]
    pub default: PerToolLimitYaml,
    #[serde(default)]
    pub per_tool: std::collections::BTreeMap<String, PerToolLimitYaml>,
    #[serde(default = "default_pp_max_buckets")]
    pub max_buckets: usize,
    #[serde(default = "default_pp_stale_ttl_secs")]
    pub stale_ttl_secs: u64,
    #[serde(default = "default_pp_warn_threshold")]
    pub warn_threshold: f64,
}

fn default_pp_enabled() -> bool {
    true
}
fn default_pp_default_limit() -> PerToolLimitYaml {
    PerToolLimitYaml {
        rps: 100.0,
        burst: 200.0,
    }
}
fn default_pp_max_buckets() -> usize {
    50_000
}
fn default_pp_stale_ttl_secs() -> u64 {
    300
}
fn default_pp_warn_threshold() -> f64 {
    0.8
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthConfigYaml {
    /// Refuses non-loopback bind at boot. For dev only.
    None,
    /// Constant-time-compared bearer token. `token_env` resolves to a
    /// non-empty string at boot. `tenant` (Phase 76.4) pins the
    /// principal's tenant; defaults to `"default"`.
    StaticToken {
        token_env: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant: Option<String>,
    },
    /// JWT validated against a remote JWKS endpoint. Algorithm-confusion
    /// guarded (HS+RS mix and `none` are rejected at boot).
    BearerJwt(BearerJwtConfigYaml),
    /// mTLS terminated by a reverse proxy. `from_header` mode reads the
    /// CN from a trusted header; loopback bind is enforced.
    MutualTls(MutualTlsConfigYaml),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BearerJwtConfigYaml {
    pub jwks_url: String,
    #[serde(default = "default_jwks_ttl")]
    pub jwks_ttl_secs: u64,
    #[serde(default = "default_jwks_cooldown")]
    pub jwks_refresh_cooldown_secs: u64,
    #[serde(default = "default_jwt_algs")]
    pub algorithms: Vec<String>,
    pub issuer: String,
    pub audiences: Vec<String>,
    #[serde(default = "default_tenant_claim")]
    pub tenant_claim: String,
    #[serde(default = "default_scopes_claim")]
    pub scopes_claim: String,
    #[serde(default = "default_jwt_leeway")]
    pub leeway_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum MutualTlsConfigYaml {
    /// Reverse-proxy strips mTLS, forwards CN via trusted header. The
    /// runtime refuses non-loopback bind in this mode.
    FromHeader {
        #[serde(default = "default_mtls_header")]
        header_name: String,
        cn_allowlist: Vec<String>,
        /// Phase 76.4 — optional CN → tenant remap. When absent, the
        /// CN itself must parse as a `TenantId` (so dotted CNs require
        /// a remap or are rejected with `TenantClaimMissing`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cn_to_tenant: Option<std::collections::BTreeMap<String, String>>,
    },
}

fn default_jwks_ttl() -> u64 {
    300
}
fn default_jwks_cooldown() -> u64 {
    10
}
fn default_jwt_algs() -> Vec<String> {
    vec!["RS256".into()]
}
fn default_tenant_claim() -> String {
    "tenant_id".into()
}
fn default_scopes_claim() -> String {
    "scope".into()
}
fn default_jwt_leeway() -> u64 {
    30
}
fn default_mtls_header() -> String {
    "X-Client-Cert-Cn".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PerIpRateLimitYaml {
    #[serde(default = "default_rate_rps")]
    pub rps: u32,
    #[serde(default = "default_rate_burst")]
    pub burst: u32,
}

impl Default for PerIpRateLimitYaml {
    fn default() -> Self {
        Self {
            rps: default_rate_rps(),
            burst: default_rate_burst(),
        }
    }
}

fn default_http_bind() -> std::net::SocketAddr {
    "127.0.0.1:7575".parse().unwrap()
}
fn default_http_allow_origins() -> Vec<String> {
    vec!["http://localhost".into(), "http://127.0.0.1".into()]
}
fn default_http_body_max_bytes() -> usize {
    1024 * 1024
}
fn default_http_max_in_flight() -> usize {
    500
}
fn default_http_request_timeout() -> u64 {
    30
}
fn default_http_idle() -> u64 {
    300
}
fn default_http_lifetime() -> u64 {
    86_400
}
fn default_http_max_sessions() -> usize {
    1_000
}
fn default_http_keepalive() -> u64 {
    15
}
fn default_http_max_age() -> u64 {
    600
}
fn default_http_buffer() -> usize {
    256
}
fn default_rate_rps() -> u32 {
    60
}
fn default_rate_burst() -> u32 {
    120
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            name: None,
            allowlist: Vec::new(),
            expose_proxies: false,
            auth_token_env: None,
            http: None,
        }
    }
}

fn default_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal() {
        let yaml = "mcp_server:\n  enabled: true\n";
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp_server.enabled);
        assert!(f.mcp_server.allowlist.is_empty());
        assert!(!f.mcp_server.expose_proxies);
        assert!(f.mcp_server.auth_token_env.is_none());
    }

    #[test]
    fn parses_allowlist() {
        let yaml = r#"
mcp_server:
  enabled: true
  name: "kate"
  allowlist:
    - who_am_i
    - memory_recall
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.mcp_server.name.as_deref(), Some("kate"));
        assert_eq!(f.mcp_server.allowlist.len(), 2);
        assert!(!f.mcp_server.expose_proxies);
        assert!(f.mcp_server.auth_token_env.is_none());
    }

    #[test]
    fn parses_expose_proxies_flag() {
        let yaml = r#"
mcp_server:
  enabled: true
  expose_proxies: true
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f.mcp_server.expose_proxies);
    }

    #[test]
    fn parses_auth_token_env() {
        let yaml = r#"
mcp_server:
  enabled: true
  auth_token_env: "MCP_SERVER_TOKEN"
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            f.mcp_server.auth_token_env.as_deref(),
            Some("MCP_SERVER_TOKEN")
        );
    }

    #[test]
    fn parses_http_block() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    bind: "127.0.0.1:7575"
    auth_token_env: "NEXO_MCP_HTTP_TOKEN"
    allow_origins:
      - "http://localhost"
    body_max_bytes: 1048576
    enable_legacy_sse: true
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.expect("http block parsed");
        assert!(http.enabled);
        assert_eq!(http.auth_token_env.as_deref(), Some("NEXO_MCP_HTTP_TOKEN"));
        assert_eq!(http.allow_origins, vec!["http://localhost".to_string()]);
        assert!(http.enable_legacy_sse);
        assert_eq!(http.max_sessions, 1000); // default applied
    }

    #[test]
    fn parses_http_auth_static_token() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: static_token
      token_env: "NEXO_MCP_TOKEN"
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        match http.auth.expect("auth parsed") {
            AuthConfigYaml::StaticToken { token_env, tenant } => {
                assert_eq!(token_env, "NEXO_MCP_TOKEN");
                assert!(tenant.is_none(), "tenant defaults to None when omitted");
            }
            other => panic!("expected StaticToken, got {other:?}"),
        }
    }

    #[test]
    fn parses_http_auth_bearer_jwt() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: bearer_jwt
      jwks_url: "https://idp.example.com/.well-known/jwks.json"
      issuer: "https://idp.example.com/"
      audiences: ["nexo-mcp"]
      algorithms: ["RS256", "ES256"]
      tenant_claim: "org_id"
      leeway_secs: 60
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        match http.auth.expect("auth parsed") {
            AuthConfigYaml::BearerJwt(jwt) => {
                assert_eq!(jwt.issuer, "https://idp.example.com/");
                assert_eq!(jwt.audiences, vec!["nexo-mcp".to_string()]);
                assert_eq!(jwt.algorithms, vec!["RS256", "ES256"]);
                assert_eq!(jwt.tenant_claim, "org_id");
                assert_eq!(jwt.leeway_secs, 60);
                assert_eq!(jwt.jwks_ttl_secs, 300); // default
                assert_eq!(jwt.scopes_claim, "scope"); // default
            }
            other => panic!("expected BearerJwt, got {other:?}"),
        }
    }

    #[test]
    fn parses_http_auth_mutual_tls_from_header() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: mutual_tls
      mode: from_header
      header_name: "X-Client-Cert-Cn"
      cn_allowlist:
        - "agent-1.internal"
        - "agent-2.internal"
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        match http.auth.expect("auth parsed") {
            AuthConfigYaml::MutualTls(MutualTlsConfigYaml::FromHeader {
                header_name,
                cn_allowlist,
                cn_to_tenant,
            }) => {
                assert_eq!(header_name, "X-Client-Cert-Cn");
                assert_eq!(cn_allowlist.len(), 2);
                assert!(cn_to_tenant.is_none(), "no remap when omitted");
            }
            other => panic!("expected MutualTls::FromHeader, got {other:?}"),
        }
    }

    #[test]
    fn parses_http_auth_static_token_with_tenant() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: static_token
      token_env: "NEXO_MCP_TOKEN"
      tenant: "prod-corp"
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        match http.auth.expect("auth parsed") {
            AuthConfigYaml::StaticToken { token_env, tenant } => {
                assert_eq!(token_env, "NEXO_MCP_TOKEN");
                assert_eq!(tenant.as_deref(), Some("prod-corp"));
            }
            other => panic!("expected StaticToken, got {other:?}"),
        }
    }

    #[test]
    fn parses_http_auth_mutual_tls_cn_to_tenant() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: mutual_tls
      mode: from_header
      cn_allowlist: ["agent-1.internal", "agent-2.internal"]
      cn_to_tenant:
        agent-1.internal: tenant-a
        agent-2.internal: tenant-b
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        match http.auth.expect("auth parsed") {
            AuthConfigYaml::MutualTls(MutualTlsConfigYaml::FromHeader { cn_to_tenant, .. }) => {
                let map = cn_to_tenant.expect("remap parsed");
                assert_eq!(map.get("agent-1.internal"), Some(&"tenant-a".to_string()));
                assert_eq!(map.get("agent-2.internal"), Some(&"tenant-b".to_string()));
            }
            other => panic!("expected MutualTls::FromHeader, got {other:?}"),
        }
    }

    #[test]
    fn parses_http_auth_none() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: none
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        assert!(matches!(http.auth, Some(AuthConfigYaml::None)));
    }

    #[test]
    fn parses_minimal_http_block_uses_defaults() {
        let yaml = r#"
mcp_server:
  enabled: true
  http: { enabled: true }
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        assert_eq!(http.body_max_bytes, 1024 * 1024);
        assert_eq!(http.session_idle_timeout_secs, 300);
        assert_eq!(http.per_ip_rate_limit.rps, 60);
        assert_eq!(http.per_ip_rate_limit.burst, 120);
    }

    #[test]
    fn parses_per_principal_rate_limit_full_block() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    per_principal_rate_limit:
      enabled: true
      default: { rps: 100.0, burst: 200.0 }
      per_tool:
        agent_turn:    { rps: 10.0, burst: 20.0 }
        memory_search: { rps: 50.0, burst: 100.0 }
      max_buckets: 50000
      stale_ttl_secs: 300
      warn_threshold: 0.8
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let http = f.mcp_server.http.unwrap();
        let pp = http.per_principal_rate_limit.expect("block parsed");
        assert!(pp.enabled);
        assert_eq!(pp.default.rps, 100.0);
        assert_eq!(pp.default.burst, 200.0);
        assert_eq!(pp.per_tool.len(), 2);
        assert_eq!(pp.per_tool["agent_turn"].rps, 10.0);
        assert_eq!(pp.max_buckets, 50_000);
        assert_eq!(pp.stale_ttl_secs, 300);
        assert!((pp.warn_threshold - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_per_principal_rate_limit_minimal_block_uses_defaults() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    per_principal_rate_limit: {}
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let pp = f.mcp_server.http.unwrap().per_principal_rate_limit.unwrap();
        assert!(pp.enabled, "default enabled is true");
        assert_eq!(pp.default.rps, 100.0);
        assert_eq!(pp.default.burst, 200.0);
        assert!(pp.per_tool.is_empty());
        assert_eq!(pp.max_buckets, 50_000);
        assert_eq!(pp.stale_ttl_secs, 300);
        assert!((pp.warn_threshold - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_per_principal_concurrency_full_block() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    per_principal_concurrency:
      enabled: true
      default: { max_in_flight: 10 }
      per_tool:
        agent_turn:    { max_in_flight: 5,  timeout_secs: 300 }
        memory_search: { max_in_flight: 20, timeout_secs: 5 }
      default_timeout_secs: 30
      queue_wait_ms: 5000
      max_buckets: 50000
      stale_ttl_secs: 300
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let pc = f
            .mcp_server
            .http
            .unwrap()
            .per_principal_concurrency
            .expect("block parsed");
        assert!(pc.enabled);
        assert_eq!(pc.default.max_in_flight, 10);
        assert_eq!(pc.per_tool.len(), 2);
        assert_eq!(pc.per_tool["agent_turn"].max_in_flight, 5);
        assert_eq!(pc.per_tool["agent_turn"].timeout_secs, Some(300));
        assert_eq!(pc.per_tool["memory_search"].timeout_secs, Some(5));
        assert_eq!(pc.default_timeout_secs, 30);
        assert_eq!(pc.queue_wait_ms, 5_000);
        assert_eq!(pc.max_buckets, 50_000);
        assert_eq!(pc.stale_ttl_secs, 300);
    }

    #[test]
    fn parses_per_principal_concurrency_minimal_block_uses_defaults() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    per_principal_concurrency: {}
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        let pc = f
            .mcp_server
            .http
            .unwrap()
            .per_principal_concurrency
            .unwrap();
        assert!(pc.enabled);
        assert_eq!(pc.default.max_in_flight, 10);
        assert!(pc.per_tool.is_empty());
        assert_eq!(pc.default_timeout_secs, 30);
        assert_eq!(pc.queue_wait_ms, 5_000);
        assert_eq!(pc.max_buckets, 50_000);
    }

    #[test]
    fn omitted_per_principal_concurrency_block_remains_none() {
        let yaml = r#"
mcp_server:
  enabled: true
  http: { enabled: true }
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f
            .mcp_server
            .http
            .unwrap()
            .per_principal_concurrency
            .is_none());
    }

    #[test]
    fn omitted_per_principal_block_remains_none() {
        let yaml = r#"
mcp_server:
  enabled: true
  http: { enabled: true }
"#;
        let f: McpServerConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(f
            .mcp_server
            .http
            .unwrap()
            .per_principal_rate_limit
            .is_none());
    }
}
