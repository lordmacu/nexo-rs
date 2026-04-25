//! `agent ext doctor --runtime` — probes each discovered extension's
//! transport (spawn + handshake + `tools/list` for stdio, beacon wait for nats, HEAD
//! for http with GET fallback when HEAD is rejected) with bounded timeouts
//! and reports the outcome.
//!
//! The static (manifest-only) doctor stays in `commands::run_doctor`;
//! this module runs *after* it when the `--runtime` flag is set.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use serde::Serialize;

use nexo_config::types::extensions::ExtensionsDoctorConfig;

use crate::discovery::{ExtensionCandidate, ExtensionDiscovery};
use crate::manifest::Transport;
use crate::runtime::{StdioRuntime, StdioSpawnOptions};

use super::status::resolve_status_for_candidate;
use super::{CliContext, CliError};

#[derive(Clone, Copy)]
pub struct DoctorOptions {
    pub runtime: bool,
    pub json: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Fail,
    Skip,
}

#[derive(Debug, Serialize)]
pub struct RuntimeCheckResult {
    pub id: String,
    pub transport: &'static str,
    pub outcome: Outcome,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub ok: usize,
    pub fail: usize,
    pub skip: usize,
}

#[derive(Debug, Serialize)]
struct DoctorRuntimeReport<'a> {
    results: &'a [RuntimeCheckResult],
    summary: Summary,
}

/// Minimal broker surface the doctor needs. Implemented by the binary
/// against `nexo_broker::AnyBroker::Nats`; for the local broker the
/// binary passes `None` and nats checks skip.
#[async_trait::async_trait]
pub trait BrokerClientForDoctor: Send + Sync {
    async fn wait_for_subject(&self, subject: &str, timeout: Duration) -> anyhow::Result<()>;
}

pub async fn run_doctor_runtime(
    ctx: CliContext<'_>,
    opts: DoctorOptions,
    broker: Option<Arc<dyn BrokerClientForDoctor>>,
) -> Result<(), CliError> {
    let cfg = ctx.extensions.doctor.clone();
    let discovery = build_discovery(&ctx.extensions);
    let report = discovery.discover();

    let candidates: Vec<_> = report.candidates.into_iter().collect();
    let disabled = ctx.extensions.disabled.clone();

    let tasks = candidates.into_iter().map(|c| {
        let id = c.manifest.id().to_string();
        let disabled_ref = disabled.clone();
        let broker = broker.clone();
        let cfg = cfg.clone();
        async move {
            let status = resolve_status_for_candidate(&id, &disabled_ref);
            if matches!(status, super::CliStatus::Disabled) {
                return RuntimeCheckResult {
                    id,
                    transport: transport_label(&c.manifest.transport),
                    outcome: Outcome::Skip,
                    elapsed_ms: 0,
                    error: Some("disabled".into()),
                };
            }
            check_one(&c, broker, &cfg).await
        }
    });

    let results: Vec<RuntimeCheckResult> = stream::iter(tasks)
        .buffer_unordered(cfg.concurrency.max(1) as usize)
        .collect()
        .await;

    let summary = summarize(&results);

    if opts.json {
        let body = DoctorRuntimeReport {
            results: &results,
            summary: Summary {
                ok: summary.ok,
                fail: summary.fail,
                skip: summary.skip,
            },
        };
        serde_json::to_writer_pretty(&mut *ctx.out, &body)
            .map_err(|e| CliError::ConfigWrite(format!("json: {e}")))?;
        writeln!(ctx.out)?;
    } else {
        writeln!(ctx.out, "runtime checks:")?;
        writeln!(
            ctx.out,
            "{:<20} {:<9} {:<7} {:>10}  ERROR",
            "ID", "TRANSPORT", "OUTCOME", "ELAPSED"
        )?;
        let mut sorted = results.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        for r in &sorted {
            let elapsed = format!("{}ms", r.elapsed_ms);
            let err = r.error.as_deref().unwrap_or("");
            writeln!(
                ctx.out,
                "{:<20} {:<9} {:<7} {:>10}  {}",
                r.id,
                r.transport,
                match r.outcome {
                    Outcome::Ok => "ok",
                    Outcome::Fail => "fail",
                    Outcome::Skip => "skip",
                },
                elapsed,
                err
            )?;
        }
        writeln!(
            ctx.out,
            "{} ok, {} fail, {} skip",
            summary.ok, summary.fail, summary.skip
        )?;
    }

    if summary.fail > 0 {
        return Err(CliError::RuntimeCheckFailed(summary.fail));
    }
    Ok(())
}

async fn check_one(
    candidate: &ExtensionCandidate,
    broker: Option<Arc<dyn BrokerClientForDoctor>>,
    cfg: &ExtensionsDoctorConfig,
) -> RuntimeCheckResult {
    let id = candidate.manifest.id().to_string();
    let transport = transport_label(&candidate.manifest.transport);
    let started = Instant::now();
    let (outcome, error) = match &candidate.manifest.transport {
        Transport::Stdio { .. } => {
            check_stdio(candidate, Duration::from_millis(cfg.stdio_timeout_ms)).await
        }
        Transport::Nats { subject_prefix } => {
            check_nats(
                &id,
                subject_prefix,
                broker,
                Duration::from_millis(cfg.nats_timeout_ms),
            )
            .await
        }
        Transport::Http { url } => {
            check_http(url, Duration::from_millis(cfg.http_timeout_ms)).await
        }
    };
    RuntimeCheckResult {
        id,
        transport,
        outcome,
        elapsed_ms: started.elapsed().as_millis() as u64,
        error,
    }
}

async fn check_stdio(
    candidate: &ExtensionCandidate,
    timeout: Duration,
) -> (Outcome, Option<String>) {
    let opts = StdioSpawnOptions {
        cwd: candidate.root_dir.clone(),
        handshake_timeout: timeout,
        shutdown_grace: Duration::from_secs(1),
        max_restart_attempts: 0,
        ..Default::default()
    };
    match StdioRuntime::spawn_with(&candidate.manifest, opts).await {
        Ok(rt) => {
            let tools_list = rt.tools_list().await;
            rt.shutdown().await;
            match tools_list {
                Ok(_) => (Outcome::Ok, None),
                Err(e) => (Outcome::Fail, Some(format!("tools/list failed: {e}"))),
            }
        }
        Err(e) => (Outcome::Fail, Some(e.to_string())),
    }
}

async fn check_nats(
    id: &str,
    subject_prefix: &str,
    broker: Option<Arc<dyn BrokerClientForDoctor>>,
    timeout: Duration,
) -> (Outcome, Option<String>) {
    let Some(broker) = broker else {
        return (Outcome::Skip, Some("no nats broker configured".into()));
    };
    let subject = format!("{subject_prefix}.{id}.beacon");
    match broker.wait_for_subject(&subject, timeout).await {
        Ok(()) => (Outcome::Ok, None),
        Err(e) => (Outcome::Fail, Some(e.to_string())),
    }
}

async fn check_http(url: &str, timeout: Duration) -> (Outcome, Option<String>) {
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => return (Outcome::Fail, Some(format!("reqwest build: {e}"))),
    };
    match client.head(url).send().await {
        Ok(resp) => {
            let s = resp.status();
            if s.is_success() || s.is_redirection() {
                return (Outcome::Ok, None);
            }
            // Some servers reject HEAD (405/501) but serve GET.
            if s == reqwest::StatusCode::METHOD_NOT_ALLOWED
                || s == reqwest::StatusCode::NOT_IMPLEMENTED
            {
                match client.get(url).send().await {
                    Ok(get_resp) => {
                        let gs = get_resp.status();
                        if gs.is_success() || gs.is_redirection() {
                            (Outcome::Ok, None)
                        } else {
                            (
                                Outcome::Fail,
                                Some(format!(
                                    "HTTP {} (HEAD) then HTTP {} (GET)",
                                    s.as_u16(),
                                    gs.as_u16()
                                )),
                            )
                        }
                    }
                    Err(e) => (
                        Outcome::Fail,
                        Some(format!("HTTP {} (HEAD) then GET error: {e}", s.as_u16())),
                    ),
                }
            } else {
                (Outcome::Fail, Some(format!("HTTP {}", s.as_u16())))
            }
        }
        Err(e) => (Outcome::Fail, Some(e.to_string())),
    }
}

fn transport_label(t: &Transport) -> &'static str {
    match t {
        Transport::Stdio { .. } => "stdio",
        Transport::Nats { .. } => "nats",
        Transport::Http { .. } => "http",
    }
}

fn build_discovery(cfg: &nexo_config::ExtensionsConfig) -> ExtensionDiscovery {
    let search_paths: Vec<PathBuf> = cfg.search_paths.iter().map(PathBuf::from).collect();
    ExtensionDiscovery::new(
        search_paths,
        cfg.ignore_dirs.clone(),
        Vec::new(),
        cfg.allowlist.clone(),
        cfg.max_depth,
    )
    .with_follow_links(cfg.follow_links)
}

pub(crate) fn summarize(results: &[RuntimeCheckResult]) -> Summary {
    let mut s = Summary {
        ok: 0,
        fail: 0,
        skip: 0,
    };
    for r in results {
        match r.outcome {
            Outcome::Ok => s.ok += 1,
            Outcome::Fail => s.fail += 1,
            Outcome::Skip => s.skip += 1,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Outcome::Ok).unwrap(), "\"ok\"");
        assert_eq!(serde_json::to_string(&Outcome::Fail).unwrap(), "\"fail\"");
        assert_eq!(serde_json::to_string(&Outcome::Skip).unwrap(), "\"skip\"");
    }

    #[test]
    fn summary_counts_buckets() {
        let results = vec![
            RuntimeCheckResult {
                id: "a".into(),
                transport: "stdio",
                outcome: Outcome::Ok,
                elapsed_ms: 10,
                error: None,
            },
            RuntimeCheckResult {
                id: "b".into(),
                transport: "stdio",
                outcome: Outcome::Fail,
                elapsed_ms: 50,
                error: Some("x".into()),
            },
            RuntimeCheckResult {
                id: "c".into(),
                transport: "nats",
                outcome: Outcome::Skip,
                elapsed_ms: 0,
                error: Some("no broker".into()),
            },
            RuntimeCheckResult {
                id: "d".into(),
                transport: "http",
                outcome: Outcome::Ok,
                elapsed_ms: 5,
                error: None,
            },
        ];
        let s = summarize(&results);
        assert_eq!(s.ok, 2);
        assert_eq!(s.fail, 1);
        assert_eq!(s.skip, 1);
    }

    #[tokio::test]
    async fn check_nats_skip_without_broker() {
        let (out, err) = check_nats("x", "ext", None, Duration::from_millis(10)).await;
        assert_eq!(out, Outcome::Skip);
        assert!(err.unwrap().contains("no nats broker"));
    }

    #[tokio::test]
    async fn check_http_fail_on_unreachable() {
        // Port 1 is privileged and almost always closed. Connection refused
        // short-circuits well before the timeout.
        let (out, err) = check_http("http://127.0.0.1:1/", Duration::from_millis(500)).await;
        assert_eq!(out, Outcome::Fail);
        assert!(err.is_some());
    }

    #[tokio::test]
    async fn check_http_falls_back_to_get_when_head_rejected() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/no-head"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/no-head"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let (out, err) = check_http(
            &format!("{}/no-head", server.uri()),
            Duration::from_millis(1000),
        )
        .await;
        assert_eq!(out, Outcome::Ok);
        assert!(err.is_none());
    }
}
