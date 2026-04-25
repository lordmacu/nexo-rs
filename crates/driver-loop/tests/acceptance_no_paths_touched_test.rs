//! `NoPathsTouched` against a real (tiny) git repo. Skipped when
//! `git` isn't on PATH so CI on minimal images still passes.

#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use nexo_driver_loop::CustomVerifier;
use nexo_driver_loop::{NoPathsTouched, ShellRunner};

async fn sh(cmd: &str, cwd: &Path) {
    let runner = ShellRunner::default();
    let r = runner.run(cmd, cwd, Duration::from_secs(15)).await.unwrap();
    assert!(r.exit_code == Some(0), "{cmd} failed: {:?}", r);
}

fn git_available() -> bool {
    which::which("git").is_ok()
}

#[tokio::test]
async fn fails_when_prefix_path_changes() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    sh(
        "git init -q && git config user.email t@t && git config user.name t",
        p,
    )
    .await;
    sh(
        "mkdir -p src secrets && echo hello > src/lib.rs && echo seed > secrets/sec.yaml",
        p,
    )
    .await;
    sh("git add -A && git commit -qm baseline", p).await;

    // Now modify a secret.
    sh("echo changed > secrets/sec.yaml", p).await;
    sh("git add -A", p).await;

    let v = NoPathsTouched::default();
    let args = serde_json::json!({"prefixes": ["secrets/"]});
    let r = v.verify(&args, p).await.unwrap();
    assert!(r.is_some(), "expected failure when secrets/ touched");
    assert!(r.unwrap().contains("secrets/"));
}

#[tokio::test]
async fn passes_when_only_safe_paths_change() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    sh(
        "git init -q && git config user.email t@t && git config user.name t",
        p,
    )
    .await;
    sh(
        "mkdir -p src secrets && echo hello > src/lib.rs && echo seed > secrets/sec.yaml",
        p,
    )
    .await;
    sh("git add -A && git commit -qm baseline", p).await;

    sh("echo changed > src/lib.rs", p).await;
    sh("git add -A", p).await;

    let v = NoPathsTouched::default();
    let args = serde_json::json!({"prefixes": ["secrets/"]});
    let r = v.verify(&args, p).await.unwrap();
    assert!(r.is_none(), "expected pass: {r:?}");
}
