// Phase 27.1 — emit build-time stamps consumed by `nexo version`
// (verbose `--version`). Keeps cost cheap on incremental builds via
// `rerun-if-changed=.git/HEAD`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=NEXO_BUILD_CHANNEL");

    let sha = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NEXO_BUILD_GIT_SHA={sha}");

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".into());
    println!("cargo:rustc-env=NEXO_BUILD_TARGET_TRIPLE={target}");

    let channel = std::env::var("NEXO_BUILD_CHANNEL").unwrap_or_else(|_| "source".into());
    println!("cargo:rustc-env=NEXO_BUILD_CHANNEL={channel}");

    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    println!("cargo:rustc-env=NEXO_BUILD_TIMESTAMP={ts}");
}
