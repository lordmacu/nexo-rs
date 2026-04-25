//! HTTP handler for `/admin/pollers/*`. Returns
//! `(status, body, content_type)` so `main.rs` can plug it into the
//! existing loopback admin dispatch chain alongside
//! `nexo-core::AgentsDirectory` and `nexo-auth` reload.

use std::sync::Arc;

use serde::Serialize;
use serde_json::json;

use crate::PollerRunner;

const JSON: &str = "application/json";

#[derive(Serialize)]
pub struct JobView {
    pub id: String,
    pub kind: String,
    pub agent: String,
    pub paused: bool,
    pub last_run_at_ms: Option<i64>,
    pub next_run_at_ms: Option<i64>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_errors: i64,
    pub items_seen_total: i64,
    pub items_dispatched_total: i64,
}

/// Dispatch a request hitting `/admin/pollers/...`. Returns `None` when
/// the path does not belong to this subsystem so the parent dispatcher
/// can fall through.
pub async fn dispatch(
    runner: &Arc<PollerRunner>,
    method: &str,
    path: &str,
    config_dir: &std::path::Path,
) -> Option<(u16, String, &'static str)> {
    let m = method.to_uppercase();

    if path == "/admin/pollers" && m == "GET" {
        return Some(list(runner).await);
    }
    if path == "/admin/pollers/reload" && m == "POST" {
        return Some(reload(runner, config_dir).await);
    }
    if let Some(rest) = path.strip_prefix("/admin/pollers/") {
        // /admin/pollers/<id>(/run|/pause|/resume|/reset)
        if let Some((id, action)) = rest.split_once('/') {
            return Some(action_endpoint(runner, &m, id, action).await);
        }
        if m == "GET" {
            return Some(get_one(runner, rest).await);
        }
    }
    None
}

async fn list(runner: &Arc<PollerRunner>) -> (u16, String, &'static str) {
    match runner.list_jobs().await {
        Ok(jobs) => (200, serde_json::to_string_pretty(&jobs).unwrap_or_default(), JSON),
        Err(e) => (500, json!({"error": e.to_string()}).to_string(), JSON),
    }
}

async fn get_one(runner: &Arc<PollerRunner>, id: &str) -> (u16, String, &'static str) {
    match runner.list_jobs().await {
        Ok(jobs) => match jobs.into_iter().find(|j| j.id == id) {
            Some(j) => (200, serde_json::to_string_pretty(&j).unwrap_or_default(), JSON),
            None => (
                404,
                json!({"error": format!("job '{id}' not found")}).to_string(),
                JSON,
            ),
        },
        Err(e) => (500, json!({"error": e.to_string()}).to_string(), JSON),
    }
}

async fn action_endpoint(
    runner: &Arc<PollerRunner>,
    method: &str,
    id: &str,
    action: &str,
) -> (u16, String, &'static str) {
    if method != "POST" {
        return (
            405,
            json!({"error": "POST required"}).to_string(),
            JSON,
        );
    }
    match action {
        "run" => match runner.run_once(id).await {
            Ok(o) => {
                let body = json!({
                    "ok": true,
                    "items_seen": o.items_seen,
                    "items_dispatched": o.items_dispatched,
                    "deliveries": o.deliver.len(),
                });
                (200, body.to_string(), JSON)
            }
            Err(e) => (
                400,
                json!({"ok": false, "error": e.to_string()}).to_string(),
                JSON,
            ),
        },
        "pause" => match runner.set_paused(id, true).await {
            Ok(()) => (200, json!({"ok": true, "paused": true}).to_string(), JSON),
            Err(e) => (
                400,
                json!({"ok": false, "error": e.to_string()}).to_string(),
                JSON,
            ),
        },
        "resume" => match runner.set_paused(id, false).await {
            Ok(()) => (200, json!({"ok": true, "paused": false}).to_string(), JSON),
            Err(e) => (
                400,
                json!({"ok": false, "error": e.to_string()}).to_string(),
                JSON,
            ),
        },
        "reset" => match runner.reset_cursor(id).await {
            Ok(()) => (200, json!({"ok": true, "reset": true}).to_string(), JSON),
            Err(e) => (
                400,
                json!({"ok": false, "error": e.to_string()}).to_string(),
                JSON,
            ),
        },
        other => (
            404,
            json!({"error": format!("unknown action '{other}'")}).to_string(),
            JSON,
        ),
    }
}

async fn reload(
    runner: &Arc<PollerRunner>,
    config_dir: &std::path::Path,
) -> (u16, String, &'static str) {
    use nexo_config::types::pollers::PollersConfigFile;
    let path = config_dir.join("pollers.yaml");
    if !path.exists() {
        return (
            404,
            json!({"error": format!("{} not found", path.display())}).to_string(),
            JSON,
        );
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (500, json!({"error": e.to_string()}).to_string(), JSON),
    };
    let resolved = match nexo_config::env::resolve_placeholders(&raw, "pollers.yaml") {
        Ok(s) => s,
        Err(e) => return (400, json!({"error": e.to_string()}).to_string(), JSON),
    };
    let file: PollersConfigFile = match serde_yaml::from_str(&resolved) {
        Ok(f) => f,
        Err(e) => return (400, json!({"error": e.to_string()}).to_string(), JSON),
    };
    match runner.reload(file.pollers).await {
        Ok(plan) => (
            200,
            json!({
                "ok": true,
                "add": plan.add,
                "replace": plan.replace,
                "remove": plan.remove,
                "keep": plan.keep,
            })
            .to_string(),
            JSON,
        ),
        Err(e) => (
            400,
            json!({"ok": false, "error": format!("{e:#}")}).to_string(),
            JSON,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poller::{PollContext, Poller, TickOutcome};
    use crate::{PollState, PollerError, PollerRunner};
    use nexo_auth::resolver::CredentialStores;
    use nexo_auth::{AgentCredentialResolver, BreakerRegistry, CredentialsBundle};
    use nexo_broker::AnyBroker;
    use nexo_config::types::pollers::{PollerJob, PollersConfig};
    use async_trait::async_trait;

    struct Mock;

    #[async_trait]
    impl Poller for Mock {
        fn kind(&self) -> &'static str { "mock" }
        async fn tick(&self, _ctx: &PollContext) -> Result<TickOutcome, PollerError> {
            Ok(TickOutcome::default())
        }
    }

    fn empty_creds() -> Arc<CredentialsBundle> {
        Arc::new(CredentialsBundle {
            stores: CredentialStores::empty(),
            resolver: Arc::new(AgentCredentialResolver::empty()),
            breakers: Arc::new(BreakerRegistry::default()),
            warnings: Vec::new(),
        })
    }

    fn job(id: &str) -> PollerJob {
        PollerJob {
            id: id.into(),
            kind: "mock".into(),
            agent: "ana".into(),
            schedule: serde_yaml::from_str("every_secs: 60").unwrap(),
            config: serde_yaml::Value::Null,
            failure_to: None,
            paused_on_boot: false,
            extra: Default::default(),
        }
    }

    async fn build_runner(jobs: Vec<PollerJob>) -> Arc<PollerRunner> {
        let cfg = PollersConfig {
            jobs,
            ..PollersConfig::default()
        };
        let state = Arc::new(PollState::open_in_memory().await.unwrap());
        let runner =
            Arc::new(PollerRunner::new(cfg, state, AnyBroker::local(), empty_creds()));
        runner.register(Arc::new(Mock));
        runner
    }

    #[tokio::test]
    async fn list_returns_jobs() {
        let runner = build_runner(vec![job("ana_leads")]).await;
        let (status, body, _) =
            dispatch(&runner, "GET", "/admin/pollers", std::path::Path::new("."))
                .await
                .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("ana_leads"));
    }

    #[tokio::test]
    async fn unknown_id_returns_404() {
        let runner = build_runner(vec![]).await;
        let (status, _, _) = dispatch(
            &runner,
            "GET",
            "/admin/pollers/nope",
            std::path::Path::new("."),
        )
        .await
        .unwrap();
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn pause_then_resume() {
        let runner = build_runner(vec![job("a")]).await;
        let (s, _, _) = dispatch(
            &runner,
            "POST",
            "/admin/pollers/a/pause",
            std::path::Path::new("."),
        )
        .await
        .unwrap();
        assert_eq!(s, 200);
        let (s, _, _) = dispatch(
            &runner,
            "POST",
            "/admin/pollers/a/resume",
            std::path::Path::new("."),
        )
        .await
        .unwrap();
        assert_eq!(s, 200);
    }

    #[tokio::test]
    async fn run_action_dispatches_to_run_once() {
        let runner = build_runner(vec![job("a")]).await;
        let (s, body, _) = dispatch(
            &runner,
            "POST",
            "/admin/pollers/a/run",
            std::path::Path::new("."),
        )
        .await
        .unwrap();
        assert_eq!(s, 200);
        assert!(body.contains("\"ok\":true"));
    }

    #[tokio::test]
    async fn unknown_action_returns_404() {
        let runner = build_runner(vec![job("a")]).await;
        let (s, _, _) = dispatch(
            &runner,
            "POST",
            "/admin/pollers/a/explode",
            std::path::Path::new("."),
        )
        .await
        .unwrap();
        assert_eq!(s, 404);
    }

    #[tokio::test]
    async fn unrelated_path_returns_none() {
        let runner = build_runner(vec![]).await;
        let r = dispatch(
            &runner,
            "GET",
            "/admin/agents",
            std::path::Path::new("."),
        )
        .await;
        assert!(r.is_none());
    }
}
