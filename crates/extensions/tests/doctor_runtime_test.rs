//! Integration tests for `agent ext doctor --runtime`.
//!
//! Covers:
//! * stdio happy path (uses the `echo_ext` example as a live extension).
//! * stdio fail when the command is missing.
//! * nats skip when no broker is injected.
//! * nats ok when a fake broker resolves the beacon wait.
//! * http ok via wiremock HEAD.
//! * http fallback HEAD->GET when endpoint rejects HEAD.
//! * disabled extensions are marked `skip`.
//! * JSON output shape.
//! * `RuntimeCheckFailed` with exit code 9 when any result is `fail`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_config::ExtensionsConfig;
use agent_config::types::extensions::ExtensionsDoctorConfig;
use agent_extensions::cli::{
    run_doctor_runtime, BrokerClientForDoctor, CliContext, CliError, DoctorOptions,
};
use async_trait::async_trait;
use tempfile::TempDir;

/// Build the `echo_ext` example on demand so `cargo test` alone is
/// sufficient — no out-of-band `cargo build --example echo_ext`.
fn echo_ext_path() -> PathBuf {
    use std::sync::OnceLock;
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("target")
                .join("debug")
                .join("examples")
                .join("echo_ext");
            if !target.exists() {
                let status = std::process::Command::new(env!("CARGO"))
                    .args([
                        "build",
                        "--quiet",
                        "-p",
                        "agent-extensions",
                        "--example",
                        "echo_ext",
                    ])
                    .status()
                    .expect("spawn cargo build --example echo_ext");
                assert!(status.success(), "cargo build --example echo_ext failed");
            }
            target
        })
        .clone()
}

fn write_plugin_toml(dir: &Path, id: &str, kind_block: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        format!(
            r#"
[plugin]
id = "{id}"
version = "0.1.0"

[capabilities]
tools = ["echo"]

{kind_block}
"#
        ),
    )
    .unwrap();
}

struct Env {
    _tmp: TempDir,
    config_dir: PathBuf,
    search_root: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join("config");
        let search_root = tmp.path().join("exts");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&search_root).unwrap();
        Env {
            _tmp: tmp,
            config_dir,
            search_root,
        }
    }

    fn add_stdio_ext(&self, id: &str, command: &str) {
        let dir = self.search_root.join(id);
        write_plugin_toml(
            &dir,
            id,
            &format!("[transport]\nkind = \"stdio\"\ncommand = \"{command}\""),
        );
    }

    fn add_nats_ext(&self, id: &str, subject_prefix: &str) {
        let dir = self.search_root.join(id);
        write_plugin_toml(
            &dir,
            id,
            &format!(
                "[transport]\nkind = \"nats\"\nsubject_prefix = \"{subject_prefix}\""
            ),
        );
    }

    fn add_http_ext(&self, id: &str, url: &str) {
        let dir = self.search_root.join(id);
        write_plugin_toml(
            &dir,
            id,
            &format!("[transport]\nkind = \"http\"\nurl = \"{url}\""),
        );
    }

    fn cfg(&self, disabled: Vec<String>, doctor: ExtensionsDoctorConfig) -> ExtensionsConfig {
        ExtensionsConfig {
            search_paths: vec![self.search_root.to_string_lossy().to_string()],
            disabled,
            doctor,
            ..Default::default()
        }
    }
}

fn fast_doctor_cfg() -> ExtensionsDoctorConfig {
    ExtensionsDoctorConfig {
        stdio_timeout_ms: 3000,
        nats_timeout_ms: 300,
        http_timeout_ms: 500,
        concurrency: 4,
    }
}

async fn run(
    env: &Env,
    cfg: ExtensionsConfig,
    broker: Option<Arc<dyn BrokerClientForDoctor>>,
    json: bool,
) -> (Result<(), CliError>, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ctx = CliContext {
        config_dir: env.config_dir.clone(),
        extensions: cfg,
        out: &mut out,
        err: &mut err,
    };
    let res = run_doctor_runtime(
        ctx,
        DoctorOptions {
            runtime: true,
            json,
        },
        broker,
    )
    .await;
    (res, String::from_utf8(out).unwrap())
}

// ── brokers ───────────────────────────────────────────────────────────────────

struct OkBroker;

#[async_trait]
impl BrokerClientForDoctor for OkBroker {
    async fn wait_for_subject(&self, _subject: &str, _timeout: Duration) -> anyhow::Result<()> {
        Ok(())
    }
}

struct TimeoutBroker;

#[async_trait]
impl BrokerClientForDoctor for TimeoutBroker {
    async fn wait_for_subject(&self, _subject: &str, _timeout: Duration) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("no beacon"))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn runtime_stdio_happy_with_echo_ext() {
    let echo = echo_ext_path();
    if !echo.exists() {
        eprintln!(
            "skipping runtime_stdio_happy_with_echo_ext: echo_ext not built at {}",
            echo.display()
        );
        return;
    }
    let env = Env::new();
    env.add_stdio_ext("echo-test", &echo.display().to_string());
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, false).await;
    res.expect("doctor runtime");
    assert!(out.contains("echo-test"), "output: {out}");
    assert!(out.contains("ok"));
}

#[tokio::test]
async fn runtime_stdio_fails_on_missing_binary() {
    let env = Env::new();
    env.add_stdio_ext("missing", "/nonexistent/path/xyz123");
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, false).await;
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::RuntimeCheckFailed(1)), "{err:?}");
    assert_eq!(err.exit_code(), 9);
    assert!(out.contains("fail"));
}

#[tokio::test]
async fn runtime_nats_skip_without_broker() {
    let env = Env::new();
    env.add_nats_ext("natext", "ext");
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, false).await;
    res.expect("skip is not a fail");
    assert!(out.contains("skip"));
    assert!(out.contains("no nats broker"));
}

#[tokio::test]
async fn runtime_nats_ok_with_broker() {
    let env = Env::new();
    env.add_nats_ext("natext", "ext");
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(
        &env,
        cfg,
        Some(Arc::new(OkBroker) as Arc<dyn BrokerClientForDoctor>),
        false,
    )
    .await;
    res.expect("ok broker");
    assert!(out.contains("natext"));
    assert!(out.contains("ok"));
}

#[tokio::test]
async fn runtime_nats_fail_on_beacon_timeout() {
    let env = Env::new();
    env.add_nats_ext("natext", "ext");
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, _out) = run(
        &env,
        cfg,
        Some(Arc::new(TimeoutBroker) as Arc<dyn BrokerClientForDoctor>),
        false,
    )
    .await;
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::RuntimeCheckFailed(_)));
}

#[tokio::test]
async fn runtime_http_ok_and_fail() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let env = Env::new();
    env.add_http_ext("httpext", &format!("{}/health", server.uri()));
    env.add_http_ext("httpbad", "http://127.0.0.1:1/dead");
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, false).await;
    let err = res.unwrap_err();
    assert!(matches!(err, CliError::RuntimeCheckFailed(1)), "{err:?}");
    assert!(out.contains("httpext"));
    assert!(out.contains("httpbad"));
}

#[tokio::test]
async fn runtime_http_fallback_head_to_get() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/head-rejected"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/head-rejected"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let env = Env::new();
    env.add_http_ext("httpget", &format!("{}/head-rejected", server.uri()));
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, false).await;
    res.expect("HEAD->GET fallback should pass");
    assert!(out.contains("httpget"));
    assert!(out.contains("ok"), "output: {out}");
}

#[tokio::test]
async fn runtime_skips_disabled() {
    let env = Env::new();
    env.add_stdio_ext("weather", "/bin/true");
    let cfg = env.cfg(vec!["weather".into()], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, false).await;
    res.expect("skip only");
    assert!(out.contains("skip"));
    assert!(out.contains("disabled"));
}

#[tokio::test]
async fn json_output_shape() {
    let env = Env::new();
    env.add_nats_ext("natext", "ext");
    let cfg = env.cfg(vec![], fast_doctor_cfg());
    let (res, out) = run(&env, cfg, None, true).await;
    res.expect("skip only");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json parses");
    let results = v["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["id"], "natext");
    assert_eq!(results[0]["transport"], "nats");
    assert_eq!(results[0]["outcome"], "skip");
    assert_eq!(v["summary"]["skip"], 1);
    assert_eq!(v["summary"]["ok"], 0);
    assert_eq!(v["summary"]["fail"], 0);
}
