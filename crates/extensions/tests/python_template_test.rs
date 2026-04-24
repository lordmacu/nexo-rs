//! Phase 11.8 — integration test for the bundled Python extension template.
//! Skipped cleanly if either `python3` or the template are missing.

use std::path::PathBuf;
use std::process::Command;

use agent_extensions::{ExtensionManifest, StdioRuntime};

fn template_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR → crates/extensions
    // Walk up to workspace root, then down to extensions/template-python/
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("extensions")
        .join("template-python")
}

fn has_python3() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_template_roundtrip() {
    let dir = template_dir();
    let main_py = dir.join("main.py");
    if !main_py.exists() {
        eprintln!("skip: template-python/main.py not found at {}", main_py.display());
        return;
    }
    if !has_python3() {
        eprintln!("skip: python3 not available in PATH");
        return;
    }

    // Manifest declares `command = "./main.py"` — which only works inside a
    // cwd that contains main.py. Spawning with cwd=dir is exactly the real
    // runtime behavior (discovery sets root_dir as cwd).
    let manifest =
        ExtensionManifest::from_path(&dir.join("plugin.toml")).expect("manifest parses");

    let rt = StdioRuntime::spawn(&manifest, dir.clone())
        .await
        .expect("spawn python template");

    let hs = rt.handshake();
    let names: Vec<&str> = hs.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"ping"), "expected ping in {:?}", names);
    assert!(names.contains(&"add"), "expected add in {:?}", names);

    let out = rt
        .tools_call("add", serde_json::json!({"a": 2.5, "b": 3.5}))
        .await
        .expect("add ok");
    assert_eq!(out["sum"], serde_json::json!(6.0));

    rt.shutdown().await;
}
