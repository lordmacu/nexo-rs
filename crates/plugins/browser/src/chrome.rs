use std::time::Duration;

use anyhow::anyhow;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

use agent_config::BrowserConfig;

pub struct RunningChrome {
    pub ws_url: String,
    pub pid: u32,
    child: tokio::process::Child,
}

impl Drop for RunningChrome {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub struct ChromeLauncher;

impl ChromeLauncher {
    pub async fn launch(config: &BrowserConfig) -> anyhow::Result<RunningChrome> {
        let exe = if config.executable.is_empty() {
            find_chrome_executable().ok_or_else(|| {
                anyhow!("no Chrome/Chromium executable found — install google-chrome or chromium")
            })?
        } else {
            config.executable.clone()
        };

        let mut args = vec![
            format!("--remote-debugging-port=0"),
            format!("--user-data-dir={}", config.user_data_dir),
            format!(
                "--window-size={},{}",
                config.window_width, config.window_height
            ),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-client-side-phishing-detection".to_string(),
            "--disable-default-apps".to_string(),
            "--disable-extensions".to_string(),
            "--disable-hang-monitor".to_string(),
            "--disable-popup-blocking".to_string(),
            "--disable-prompt-on-repost".to_string(),
            "--disable-sync".to_string(),
            "--disable-translate".to_string(),
            "--metrics-recording-only".to_string(),
            "--safebrowsing-disable-auto-update".to_string(),
            "about:blank".to_string(),
        ];

        if config.headless {
            args.push("--headless=new".to_string());
            args.push("--disable-gpu".to_string());
        }

        let mut child = Command::new(&exe)
            .args(&args)
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("failed to spawn Chrome ({exe}): {e}"))?;

        let pid = child.id().unwrap_or(0);

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to capture Chrome stderr"))?;

        let ws_url = wait_for_devtools_url(stderr, config.connect_timeout_ms)
            .await
            .inspect_err(|_e| {
                let _ = child.start_kill();
            })?;

        Ok(RunningChrome { ws_url, pid, child })
    }

    /// Attach to an already-running Chrome at a CDP HTTP URL.
    pub async fn connect_existing(cdp_url: &str, timeout_ms: u64) -> anyhow::Result<String> {
        // Delegate to the canonical discovery path in cdp::client which also
        // rewrites the authority of the returned webSocketDebuggerUrl
        // (Chrome echoes the Host header and drops the real port).
        crate::cdp::CdpClient::discover_ws_url(cdp_url, timeout_ms).await
    }
}

async fn wait_for_devtools_url(
    stderr: tokio::process::ChildStderr,
    timeout_ms: u64,
) -> anyhow::Result<String> {
    let reader = tokio::io::BufReader::new(stderr);
    let mut lines = reader.lines();

    let timeout = Duration::from_millis(timeout_ms);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);

        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for Chrome DevTools URL"));
        }

        let line = tokio::time::timeout(remaining, lines.next_line())
            .await
            .map_err(|_| anyhow!("timed out waiting for Chrome DevTools URL"))?
            .map_err(|e| anyhow!("error reading Chrome stderr: {e}"))?;

        let Some(line) = line else {
            return Err(anyhow!("Chrome exited before DevTools URL appeared"));
        };

        if let Some(rest) = line.strip_prefix("DevTools listening on ") {
            return Ok(rest.trim().to_string());
        }
    }
}

fn find_chrome_executable() -> Option<String> {
    let candidates = [
        "google-chrome",
        "google-chrome-stable",
        "chromium-browser",
        "chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
        "/snap/bin/chromium",
    ];

    for candidate in &candidates {
        if which_exists(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn which_exists(name: &str) -> bool {
    // Fast check: try to find in PATH using `which` or just test if absolute path exists
    if name.starts_with('/') {
        return std::path::Path::new(name).exists();
    }
    // Search PATH
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let full = format!("{dir}/{name}");
            if std::path::Path::new(&full).exists() {
                return true;
            }
        }
    }
    false
}
