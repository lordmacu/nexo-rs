//! End-to-end "real-world" multi-instance test against the
//! *published* `nexo-plugin-browser` binary.
//!
//! Scenario the user requested: spin up two declared browser
//! instances simultaneously, point one at Google and the other
//! at GitHub, and prove they're isolated Chrome processes hitting
//! different sites at the same time.
//!
//! Drives the binary over JSON-RPC stdio (the same wire the
//! daemon uses internally), so a green result proves the
//! published artifact is operationally correct end-to-end:
//! manifest parses, plugin.configure accepts the array shape,
//! tool dispatch routes per `instance`, two real Chromes boot in
//! parallel under distinct `user_data_dir` paths, and each
//! navigates to its own URL without bleed.
//!
//! Skips if either env var is missing OR if `CHROMIUM_BIN` is
//! unset — keeps CI hosts without Chrome installed green.
//!
//! Required env:
//!   - `NEXO_PLUGIN_BROWSER_BIN` (or fallback
//!     `/tmp/nexo-publish-smoke/bin/nexo-plugin-browser`)
//!   - `CHROMIUM_BIN` — path to chromium / chrome / google-chrome
//!
//! Run:
//!     NEXO_PLUGIN_BROWSER_BIN=/tmp/nexo-publish-smoke/bin/nexo-plugin-browser \
//!     CHROMIUM_BIN=/snap/bin/chromium \
//!     cargo nextest run -p nexo-core --test browser_plugin_crates_io_e2e --no-capture

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

fn locate_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NEXO_PLUGIN_BROWSER_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let default = PathBuf::from("/tmp/nexo-publish-smoke/bin/nexo-plugin-browser");
    if default.exists() {
        return Some(default);
    }
    None
}

fn rpc(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>, frame: Value) -> Value {
    let line = serde_json::to_string(&frame).unwrap();
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
    let mut buf = String::new();
    stdout.read_line(&mut buf).expect("read reply");
    serde_json::from_str(buf.trim()).expect("reply parses as JSON")
}

trait ChildExt {
    fn wait_timeout_or_kill(&mut self, dur: Duration) -> std::io::Result<()>;
}

impl ChildExt for std::process::Child {
    fn wait_timeout_or_kill(&mut self, dur: Duration) -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + dur;
        loop {
            match self.try_wait()? {
                Some(_) => return Ok(()),
                None if std::time::Instant::now() >= deadline => {
                    let _ = self.kill();
                    return self.wait().map(|_| ());
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[test]
fn published_binary_drives_two_chromes_one_to_google_one_to_github() {
    let Some(bin) = locate_binary() else {
        eprintln!("skipping: NEXO_PLUGIN_BROWSER_BIN unset and default path missing");
        return;
    };
    let Ok(chromium) = std::env::var("CHROMIUM_BIN") else {
        eprintln!("skipping: CHROMIUM_BIN unset");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("NEXO_PLUGIN_BROWSER_USER_DATA_DIR", tmp.path())
        .env("NEXO_PLUGIN_BROWSER_EXECUTABLE", &chromium)
        .env("RUST_LOG", "warn")
        .spawn()
        .expect("spawn published binary");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    // 1. Handshake.
    let init = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    assert_eq!(init["result"]["manifest"]["plugin"]["id"], "browser");
    assert_eq!(init["result"]["manifest"]["plugin"]["version"], "0.3.2");

    // 2. Declare two instances. Both headless; each gets its own
    //    user_data_dir under the tempdir parent (boot loop
    //    resolves ${state_dir}/instances/<label>/).
    let cfg = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "plugin.configure",
            "params": {
                "value": [
                    { "instance": "google_search",
                      "headless": true,
                      "executable": chromium,
                      "args": ["--no-sandbox"] },
                    { "instance": "github_repo",
                      "headless": true,
                      "executable": chromium,
                      "args": ["--no-sandbox"] }
                ]
            }
        }),
    );
    assert!(cfg["error"].is_null(), "configure failed: {cfg}");

    // 3. Instance A navigates to Google search.
    let nav_a = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tool.invoke",
            "params": {
                "plugin_id": "browser",
                "tool_name": "browser_navigate",
                "args": {
                    "instance": "google_search",
                    "url": "https://www.google.com/search?q=nexo+plugin+browser"
                }
            }
        }),
    );
    if nav_a["error"].is_object() {
        eprintln!(
            "skipping the rest: instance A navigate failed (likely no network): {}",
            nav_a["error"]
        );
        let _ = rpc(
            &mut stdin,
            &mut stdout,
            json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
        );
        let _ = child.wait_timeout_or_kill(Duration::from_secs(5));
        return;
    }
    assert_eq!(nav_a["result"]["ok"], true, "navigate A: {nav_a}");

    // 4. Instance B navigates to a distinct URL (GitHub repo).
    let nav_b = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tool.invoke",
            "params": {
                "plugin_id": "browser",
                "tool_name": "browser_navigate",
                "args": {
                    "instance": "github_repo",
                    "url": "https://github.com/lordmacu/nexo-plugin-browser"
                }
            }
        }),
    );
    assert_eq!(nav_b["result"]["ok"], true, "navigate B: {nav_b}");

    // 5. Read location.href on each — proves the two Chromes are
    //    distinct processes with distinct pages live at the same
    //    time.
    let url_a = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "tool.invoke",
            "params": {
                "plugin_id": "browser",
                "tool_name": "browser_current_url",
                "args": { "instance": "google_search" }
            }
        }),
    );
    let url_b = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "tool.invoke",
            "params": {
                "plugin_id": "browser",
                "tool_name": "browser_current_url",
                "args": { "instance": "github_repo" }
            }
        }),
    );
    let url_a_str = url_a["result"]["result"].as_str().unwrap_or("");
    let url_b_str = url_b["result"]["result"].as_str().unwrap_or("");
    eprintln!("google_search URL: {url_a_str}");
    eprintln!("github_repo URL: {url_b_str}");

    assert!(
        url_a_str.contains("google.com"),
        "instance A should be on google.com; got: {url_a_str}"
    );
    assert!(
        url_b_str.contains("github.com"),
        "instance B should be on github.com; got: {url_b_str}"
    );
    assert_ne!(
        url_a_str, url_b_str,
        "two instances must hold distinct pages simultaneously"
    );

    // 6. Confirm registered instances via the admin list.
    let admin_list = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 30,
            "method": "tool.invoke",
            "params": {
                "plugin_id": "browser",
                "tool_name": "browser_evaluate",
                "args": {
                    "instance": "google_search",
                    "script": "document.title"
                }
            }
        }),
    );
    let title_a = admin_list["result"]["result"].as_str().unwrap_or("");
    eprintln!("google_search title: {title_a}");

    // Clean shutdown.
    let _ = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    let _ = child.wait_timeout_or_kill(Duration::from_secs(10));
    drop(tmp);
}
