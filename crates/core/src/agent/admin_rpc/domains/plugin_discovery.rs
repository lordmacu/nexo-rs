//! `nexo/admin/plugins/{search,compat_check,refresh_index}` handlers
//! (Phase 98.10).
//!
//! Three read-mostly verbs that surface the public-catalogue layer
//! (`nexo-plugin-discovery`) to the admin UI:
//!   - `search`: full catalogue with optional substring +
//!     category + source filters.
//!   - `compat_check`: per-crate manifest + SDK semver result for
//!     the pre-install dialog.
//!   - `refresh_index`: drop the 24h disk cache so the next
//!     `search` re-runs every source.
//!
//! The trio shares one `DiscoveryReader` adapter abstraction so
//! tests can inject a fake without spinning up wiremock servers.
//! Production wires `nexo_setup::discovery_adapter::
//! DefaultDiscoveryAdapter` (98.11) which delegates to
//! `nexo_plugin_discovery::client::DefaultDiscoveryClient`.

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::plugin_discovery::{
    PluginsCompatCheckParams, PluginsCompatCheckResponse, PluginsRefreshIndexParams,
    PluginsRefreshIndexResponse, PluginsSearchParams, PluginsSearchResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Adapter abstraction for the discovery layer. Production
/// implementation lives in `nexo-setup::discovery_adapter`; tests
/// inject in-memory fakes.
#[async_trait]
pub trait DiscoveryReader: Send + Sync + std::fmt::Debug {
    /// Return the merged catalogue. Apply the request's `query`,
    /// `compat_only`, `category`, `source` filters server-side
    /// (cheaper than shipping the full catalogue every call).
    async fn search(
        &self,
        params: &PluginsSearchParams,
    ) -> anyhow::Result<PluginsSearchResponse>;

    /// Resolve compat + manifest summary for one crate. `version`
    /// is informational for now (manifest fetched from HEAD); pin-
    /// to-tag is a follow-up.
    async fn compat_check(
        &self,
        params: &PluginsCompatCheckParams,
    ) -> anyhow::Result<PluginsCompatCheckResponse>;

    /// Drop the on-disk catalogue cache + return scrape diagnostics
    /// (so the operator can see which sources contributed to the
    /// next refresh and which failed).
    async fn refresh_index(&self) -> anyhow::Result<PluginsRefreshIndexResponse>;
}

// ── handlers ────────────────────────────────────────────────────

/// `nexo/admin/plugins/search`.
pub async fn search(reader: &dyn DiscoveryReader, params: Value) -> AdminRpcResult {
    let p: PluginsSearchParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    // Validate filter inputs against tight char classes; reject
    // free-form query strings only when they exceed a sane length
    // (256 chars covers any realistic substring search).
    if let Some(ref q) = p.query {
        if q.len() > 256 {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(
                "query exceeds 256 chars".into(),
            ));
        }
    }
    if let Some(ref c) = p.category {
        if !is_known_category(c) {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "unknown category `{c}` (expected channel|poller|tool|webhook|persona)"
            )));
        }
    }
    if let Some(ref s) = p.source {
        if !is_known_source(s) {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(format!(
                "unknown source `{s}` (expected crates_io|github_topic|curated_index)"
            )));
        }
    }
    match reader.search(&p).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "plugins.search: {e}"
        ))),
    }
}

/// `nexo/admin/plugins/compat_check`.
pub async fn compat_check(reader: &dyn DiscoveryReader, params: Value) -> AdminRpcResult {
    let p: PluginsCompatCheckParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_crate_name(&p.crate_name) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
    }
    if let Some(ref v) = p.version {
        if let Err(msg) = validate_version(v) {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
        }
    }
    match reader.compat_check(&p).await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "plugins.compat_check: {e}"
        ))),
    }
}

/// `nexo/admin/plugins/refresh_index`.
pub async fn refresh_index(reader: &dyn DiscoveryReader, params: Value) -> AdminRpcResult {
    if let Err(e) = serde_json::from_value::<PluginsRefreshIndexParams>(params) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
    }
    match reader.refresh_index().await {
        Ok(resp) => AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null)),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "plugins.refresh_index: {e}"
        ))),
    }
}

// ── validators ──────────────────────────────────────────────────

fn is_known_category(c: &str) -> bool {
    matches!(
        c,
        "channel" | "poller" | "tool" | "webhook" | "persona" | "unknown"
    )
}

fn is_known_source(s: &str) -> bool {
    matches!(s, "crates_io" | "github_topic" | "curated_index")
}

fn validate_crate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("crate_name is empty".into());
    }
    if name.len() > 64 {
        return Err("crate_name exceeds 64 chars".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "crate_name `{name}` contains chars outside [a-z0-9_-]"
        ));
    }
    Ok(())
}

fn validate_version(v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Err("version is empty".into());
    }
    if v.len() > 32 {
        return Err("version exceeds 32 chars".into());
    }
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '+' || c == '-')
    {
        return Err(format!(
            "version `{v}` contains chars outside [0-9A-Za-z.+-]"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::admin::plugin_discovery::{
        CompatStatus, DiscoveredPlugin, ManifestSummary, PluginCategory, PluginSource, SourceError,
        TrustTier,
    };
    use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct StubReader {
        last_search: Mutex<Option<PluginsSearchParams>>,
        last_compat: Mutex<Option<PluginsCompatCheckParams>>,
        refresh_calls: Mutex<u32>,
        next_err: Mutex<Option<String>>,
    }

    fn sample_plugin() -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: "nexo-plugin-telegram".into(),
            version: Some("0.3.0".into()),
            description: Some("Telegram channel".into()),
            owner: "lordmacu".into(),
            sources: vec![PluginSource::CratesIo],
            repo_url: None,
            homepage: None,
            tags: vec!["messaging".into()],
            category: PluginCategory::Channel,
            trust_tier: TrustTier::Official,
            compat: CompatStatus::Compatible,
            manifest_url: None,
            install_cmd: "cargo install nexo-plugin-telegram --version 0.3.0".into(),
            install_params: PluginsInstallParams {
                crate_name: "nexo-plugin-telegram".into(),
                version: Some("0.3.0".into()),
                repo: None,
                source: InstallSource::Release,
                force: false,
                require_signature: false,
                skip_signature_verify: false,
            },
        }
    }

    #[async_trait]
    impl DiscoveryReader for StubReader {
        async fn search(
            &self,
            params: &PluginsSearchParams,
        ) -> anyhow::Result<PluginsSearchResponse> {
            *self.last_search.lock().unwrap() = Some(params.clone());
            if let Some(msg) = self.next_err.lock().unwrap().take() {
                anyhow::bail!(msg);
            }
            Ok(PluginsSearchResponse {
                items: vec![sample_plugin()],
                fetched_at_ms: 12345,
                partial_failures: vec![SourceError {
                    source: "github_topic".into(),
                    message: "synthetic".into(),
                }],
            })
        }
        async fn compat_check(
            &self,
            params: &PluginsCompatCheckParams,
        ) -> anyhow::Result<PluginsCompatCheckResponse> {
            *self.last_compat.lock().unwrap() = Some(params.clone());
            Ok(PluginsCompatCheckResponse {
                compat: CompatStatus::Compatible,
                manifest_summary: Some(ManifestSummary {
                    plugin_id: "telegram".into(),
                    plugin_version: "0.3.0".into(),
                    manifest_version: 2,
                    sdk_requires: Some(">=0.1".into()),
                    category: PluginCategory::Channel,
                }),
            })
        }
        async fn refresh_index(&self) -> anyhow::Result<PluginsRefreshIndexResponse> {
            *self.refresh_calls.lock().unwrap() += 1;
            Ok(PluginsRefreshIndexResponse {
                items_count: 1,
                sources_ok: vec!["crates_io".into(), "curated_index".into()],
                sources_err: vec![SourceError {
                    source: "github_topic".into(),
                    message: "synthetic rate limit".into(),
                }],
            })
        }
    }

    #[tokio::test]
    async fn search_invokes_adapter_with_filters() {
        let stub = StubReader::default();
        let res = search(
            &stub,
            serde_json::json!({"query": "telegram", "compat_only": true}),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["items"][0]["name"], "nexo-plugin-telegram");
        assert_eq!(payload["fetched_at_ms"], 12345);
        let last = stub.last_search.lock().unwrap().clone().unwrap();
        assert_eq!(last.query.as_deref(), Some("telegram"));
        assert!(last.compat_only);
    }

    #[tokio::test]
    async fn search_rejects_unknown_category() {
        let stub = StubReader::default();
        let res = search(&stub, serde_json::json!({"category": "not_a_real_one"})).await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("unknown category"));
    }

    #[tokio::test]
    async fn search_rejects_unknown_source() {
        let stub = StubReader::default();
        let res = search(&stub, serde_json::json!({"source": "evil"})).await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("unknown source"));
    }

    #[tokio::test]
    async fn search_rejects_oversized_query() {
        let stub = StubReader::default();
        let q = "x".repeat(300);
        let res = search(&stub, serde_json::json!({ "query": q })).await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("256 chars"));
    }

    #[tokio::test]
    async fn compat_check_validates_crate_name() {
        let stub = StubReader::default();
        let res = compat_check(
            &stub,
            serde_json::json!({"crate_name": "Bad Name With Spaces"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("outside [a-z0-9_-]"));
        assert!(stub.last_compat.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn compat_check_validates_version() {
        let stub = StubReader::default();
        let res = compat_check(
            &stub,
            serde_json::json!({"crate_name": "nexo-plugin-x", "version": "1.0 ; rm -rf /"}),
        )
        .await;
        let err = res.error.expect("err");
        assert_eq!(err.code(), -32602);
        assert!(err.to_string().contains("outside"));
    }

    #[tokio::test]
    async fn compat_check_accepts_valid_params() {
        let stub = StubReader::default();
        let res = compat_check(
            &stub,
            serde_json::json!({"crate_name": "nexo-plugin-telegram", "version": "0.3.0"}),
        )
        .await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["compat"]["kind"], "compatible");
        assert_eq!(payload["manifest_summary"]["plugin_id"], "telegram");
    }

    #[tokio::test]
    async fn refresh_index_increments_adapter_counter() {
        let stub = StubReader::default();
        let res = refresh_index(&stub, serde_json::json!({})).await;
        let payload = res.result.expect("ok");
        assert_eq!(payload["items_count"], 1);
        assert_eq!(*stub.refresh_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn search_internal_error_maps_to_internal_code() {
        let stub = StubReader::default();
        *stub.next_err.lock().unwrap() = Some("upstream exploded".into());
        let res = search(&stub, serde_json::json!({})).await;
        let err = res.error.expect("err");
        // -32603 = Internal Error JSON-RPC code
        assert_eq!(err.code(), -32603);
        assert!(err.to_string().contains("upstream exploded"));
    }
}
