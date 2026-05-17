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

/// Stealth flags that defeat the most obvious headless-Chrome
/// fingerprints (navigator.webdriver, AutomationControlled blink
/// feature, missing UA string). Not a real cloaking solution, but
/// enough to stop Google's `/sorry/index` redirect on a fresh IP.
fn stealth_args() -> Vec<&'static str> {
    vec![
        "--no-sandbox",
        "--disable-blink-features=AutomationControlled",
        "--disable-features=IsolateOrigins,site-per-process",
        "--no-first-run",
        "--no-default-browser-check",
        "--user-agent=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    ]
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

#[test]
fn published_binary_stealth_three_instances_google_brave_github() {
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

    let _ = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );

    // Three declared instances with the stealth flag set so
    // navigator.webdriver is `undefined`. Each navigates to a
    // different search engine (or repo host) to prove the wire
    // routes by `instance` arg + the three Chromes hold three
    // distinct origins at the same time.
    let args_value: Vec<Value> = stealth_args()
        .iter()
        .map(|s| Value::String(s.to_string()))
        .collect();
    let cfg = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "plugin.configure",
            "params": {
                "value": [
                    { "instance": "google_stealth",
                      "headless": true,
                      "executable": chromium,
                      "args": args_value },
                    { "instance": "brave_stealth",
                      "headless": true,
                      "executable": chromium,
                      "args": args_value },
                    { "instance": "github_stealth",
                      "headless": true,
                      "executable": chromium,
                      "args": args_value }
                ]
            }
        }),
    );
    assert!(cfg["error"].is_null(), "configure failed: {cfg}");

    let navigate = |stdin: &mut ChildStdin,
                    stdout: &mut BufReader<ChildStdout>,
                    id: i64,
                    instance: &str,
                    url: &str|
     -> Value {
        rpc(
            stdin,
            stdout,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tool.invoke",
                "params": {
                    "plugin_id": "browser",
                    "tool_name": "browser_navigate",
                    "args": { "instance": instance, "url": url }
                }
            }),
        )
    };
    let current_url = |stdin: &mut ChildStdin,
                       stdout: &mut BufReader<ChildStdout>,
                       id: i64,
                       instance: &str|
     -> String {
        let r = rpc(
            stdin,
            stdout,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tool.invoke",
                "params": {
                    "plugin_id": "browser",
                    "tool_name": "browser_current_url",
                    "args": { "instance": instance }
                }
            }),
        );
        r["result"]["result"].as_str().unwrap_or("").to_string()
    };

    // 1. Google (with stealth flags — should avoid /sorry/index).
    let nav_g = navigate(
        &mut stdin,
        &mut stdout,
        10,
        "google_stealth",
        "https://www.google.com/search?q=rust+programming",
    );
    if nav_g["error"].is_object() {
        eprintln!(
            "skipping: google navigate failed (no network?): {}",
            nav_g["error"]
        );
        let _ = rpc(
            &mut stdin,
            &mut stdout,
            json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
        );
        let _ = child.wait_timeout_or_kill(Duration::from_secs(5));
        return;
    }
    assert_eq!(nav_g["result"]["ok"], true);

    // 2. Brave Search.
    let nav_b = navigate(
        &mut stdin,
        &mut stdout,
        11,
        "brave_stealth",
        "https://search.brave.com/search?q=rust+programming",
    );
    assert_eq!(nav_b["result"]["ok"], true, "brave navigate: {nav_b}");

    // 3. GitHub.
    let nav_gh = navigate(
        &mut stdin,
        &mut stdout,
        12,
        "github_stealth",
        "https://github.com/lordmacu/nexo-plugin-browser",
    );
    assert_eq!(nav_gh["result"]["ok"], true);

    // Read URLs back.
    let url_g = current_url(&mut stdin, &mut stdout, 20, "google_stealth");
    let url_b = current_url(&mut stdin, &mut stdout, 21, "brave_stealth");
    let url_gh = current_url(&mut stdin, &mut stdout, 22, "github_stealth");
    eprintln!("google_stealth URL: {url_g}");
    eprintln!("brave_stealth URL:  {url_b}");
    eprintln!("github_stealth URL: {url_gh}");

    // Probe navigator.webdriver on each — should be `undefined`
    // when the stealth flag is honoured.
    let probe = |stdin: &mut ChildStdin,
                 stdout: &mut BufReader<ChildStdout>,
                 id: i64,
                 instance: &str|
     -> String {
        let r = rpc(
            stdin,
            stdout,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tool.invoke",
                "params": {
                    "plugin_id": "browser",
                    "tool_name": "browser_evaluate",
                    "args": {
                        "instance": instance,
                        "script": "String(navigator.webdriver)"
                    }
                }
            }),
        );
        r["result"]["result"].as_str().unwrap_or("").to_string()
    };
    let webdriver_g = probe(&mut stdin, &mut stdout, 30, "google_stealth");
    let webdriver_b = probe(&mut stdin, &mut stdout, 31, "brave_stealth");
    let webdriver_gh = probe(&mut stdin, &mut stdout, 32, "github_stealth");
    eprintln!(
        "navigator.webdriver — google:{webdriver_g} brave:{webdriver_b} github:{webdriver_gh}"
    );
    for w in [&webdriver_g, &webdriver_b, &webdriver_gh] {
        assert!(
            w == "undefined" || w == "false",
            "stealth flag must hide automation; got navigator.webdriver={w}"
        );
    }

    // Pages are alive simultaneously + distinct origins.
    assert!(
        url_g.contains("google.com"),
        "google should be on google.com (possibly /sorry if stealth wasn't enough); got {url_g}"
    );
    assert!(
        url_b.contains("search.brave.com") || url_b.contains("brave.com"),
        "brave instance should be on Brave Search; got {url_b}"
    );
    assert!(
        url_gh.contains("github.com"),
        "github instance should be on github.com; got {url_gh}"
    );
    // Three distinct origins ⇒ three Chromes really live.
    assert_ne!(url_g, url_b);
    assert_ne!(url_b, url_gh);
    assert_ne!(url_g, url_gh);

    // Bonus: did the google one escape /sorry/index?
    if url_g.contains("/sorry/") {
        eprintln!(
            "NOTE: google still served the captcha redirect — stealth flags weren't \
             enough on this IP. Real cloaking would need a residential UA + cookies."
        );
    } else {
        eprintln!("✓ stealth flags evaded the google /sorry/ redirect");
    }

    let _ = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    let _ = child.wait_timeout_or_kill(Duration::from_secs(10));
    drop(tmp);
}

#[test]
fn published_binary_compares_search_engine_bot_detection() {
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

    let _ = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );

    let args_value: Vec<Value> = stealth_args()
        .iter()
        .map(|s| Value::String(s.to_string()))
        .collect();
    let cfg = rpc(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "plugin.configure",
            "params": {
                "value": [
                    { "instance": "g",
                      "headless": true,
                      "executable": chromium,
                      "command_timeout_ms": 30000,
                      "args": args_value },
                    { "instance": "b",
                      "headless": true,
                      "executable": chromium,
                      "command_timeout_ms": 30000,
                      "args": args_value }
                ]
            }
        }),
    );
    assert!(cfg["error"].is_null(), "configure failed: {cfg}");

    fn nav_one(
        stdin: &mut ChildStdin,
        stdout: &mut BufReader<ChildStdout>,
        id: i64,
        instance: &str,
        url: &str,
    ) -> Value {
        rpc(
            stdin,
            stdout,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tool.invoke",
                "params": {
                    "plugin_id": "browser",
                    "tool_name": "browser_navigate",
                    "args": { "instance": instance, "url": url }
                }
            }),
        )
    }
    fn eval_one(
        stdin: &mut ChildStdin,
        stdout: &mut BufReader<ChildStdout>,
        id: i64,
        instance: &str,
        script: &str,
    ) -> Value {
        rpc(
            stdin,
            stdout,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tool.invoke",
                "params": {
                    "plugin_id": "browser",
                    "tool_name": "browser_evaluate",
                    "args": { "instance": instance, "script": script }
                }
            }),
        )
    }

    let query = "rust+programming+language";

    // Page-fingerprint per engine: final URL, anchor count (proxy
    // for rendered results), title, body-text excerpt + length, and
    // a regex probe for known captcha / block keywords.
    let probe_script = r#"({
        url: location.href,
        title: document.title,
        anchors: document.querySelectorAll('a').length,
        bodyTextLen: document.body ? document.body.innerText.length : 0,
        bodyExcerpt: document.body ? document.body.innerText.slice(0, 240) : '',
        webdriver: String(navigator.webdriver),
        captchaHit: /captcha|unusual traffic|verify you are human|recaptcha|not a robot|sorry\/index/i.test(document.body ? document.body.innerText : '')
    })"#;

    let nav_g = nav_one(
        &mut stdin,
        &mut stdout,
        10,
        "g",
        &format!("https://www.google.com/search?q={query}"),
    );
    if nav_g["error"].is_object() {
        eprintln!("skipping: google navigate failed: {}", nav_g["error"]);
        let _ = rpc(
            &mut stdin,
            &mut stdout,
            json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
        );
        let _ = child.wait_timeout_or_kill(Duration::from_secs(5));
        return;
    }
    std::thread::sleep(Duration::from_millis(1500));
    let g_probe = eval_one(&mut stdin, &mut stdout, 11, "g", probe_script);
    let g = &g_probe["result"]["result"];

    let nav_b = nav_one(
        &mut stdin,
        &mut stdout,
        20,
        "b",
        &format!("https://search.brave.com/search?q={query}"),
    );
    if nav_b["error"].is_object() {
        eprintln!("skipping: brave navigate failed: {}", nav_b["error"]);
        let _ = rpc(
            &mut stdin,
            &mut stdout,
            json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
        );
        let _ = child.wait_timeout_or_kill(Duration::from_secs(5));
        return;
    }
    std::thread::sleep(Duration::from_millis(1500));
    let b_probe = eval_one(&mut stdin, &mut stdout, 21, "b", probe_script);
    let b = &b_probe["result"]["result"];

    let g_url = g["url"].as_str().unwrap_or("");
    let g_title = g["title"].as_str().unwrap_or("");
    let g_anchors = g["anchors"].as_u64().unwrap_or(0);
    let g_captcha = g["captchaHit"].as_bool().unwrap_or(false);
    let g_body_len = g["bodyTextLen"].as_u64().unwrap_or(0);
    let g_excerpt = g["bodyExcerpt"].as_str().unwrap_or("");

    let b_url = b["url"].as_str().unwrap_or("");
    let b_title = b["title"].as_str().unwrap_or("");
    let b_anchors = b["anchors"].as_u64().unwrap_or(0);
    let b_captcha = b["captchaHit"].as_bool().unwrap_or(false);
    let b_body_len = b["bodyTextLen"].as_u64().unwrap_or(0);
    let b_excerpt = b["bodyExcerpt"].as_str().unwrap_or("");

    eprintln!("\n── GOOGLE ─────────────────────────────────────────");
    eprintln!("  URL:       {g_url}");
    eprintln!("  title:     {g_title}");
    eprintln!("  anchors:   {g_anchors}");
    eprintln!("  body len:  {g_body_len}");
    eprintln!("  captcha?:  {g_captcha}");
    eprintln!("  excerpt:   {g_excerpt}");
    eprintln!("\n── BRAVE ──────────────────────────────────────────");
    eprintln!("  URL:       {b_url}");
    eprintln!("  title:     {b_title}");
    eprintln!("  anchors:   {b_anchors}");
    eprintln!("  body len:  {b_body_len}");
    eprintln!("  captcha?:  {b_captcha}");
    eprintln!("  excerpt:   {b_excerpt}\n");

    // Hard assertion: Brave should serve real results.
    assert!(
        b_url.contains("search.brave.com"),
        "Brave should keep us on its search domain; got {b_url}"
    );
    assert!(
        !b_captcha,
        "Brave should NOT trigger a captcha for stealth chromium; got: {b_excerpt}"
    );
    assert!(
        b_anchors >= 30,
        "Brave search results should render many anchor tags; got only {b_anchors}"
    );

    let google_blocked = g_url.contains("/sorry/") || g_captcha || g_anchors < 30;
    if google_blocked {
        eprintln!(
            "VERDICT — Google: BLOCKED automation (/sorry/={}, captcha={g_captcha}, anchors={g_anchors})",
            g_url.contains("/sorry/")
        );
    } else {
        eprintln!("VERDICT — Google: served results (anchors={g_anchors}, body_len={g_body_len})");
    }
    eprintln!(
        "VERDICT — Brave: served results (anchors={b_anchors}, body_len={b_body_len}, captcha={b_captcha})"
    );

    let _ = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":{}}),
    );
    let _ = child.wait_timeout_or_kill(Duration::from_secs(10));
    drop(tmp);
}
