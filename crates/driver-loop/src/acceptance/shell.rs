//! Cross-platform-ish (Unix-default) shell runner used by the
//! shell-command criterion and the built-in custom verifiers.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;

use crate::error::DriverError;

#[derive(Clone, Debug)]
pub struct ShellRunner {
    shell: PathBuf,
    output_byte_limit: usize,
    forced_kill_after: Duration,
}

#[derive(Clone, Debug)]
pub struct ShellResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration: Duration,
}

impl Default for ShellRunner {
    fn default() -> Self {
        Self {
            shell: PathBuf::from("/bin/sh"),
            output_byte_limit: 1024 * 1024,
            forced_kill_after: Duration::from_secs(1),
        }
    }
}

impl ShellRunner {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_shell(mut self, p: impl Into<PathBuf>) -> Self {
        self.shell = p.into();
        self
    }
    pub fn with_output_byte_limit(mut self, n: usize) -> Self {
        self.output_byte_limit = n;
        self
    }
    pub fn with_forced_kill_after(mut self, d: Duration) -> Self {
        self.forced_kill_after = d;
        self
    }

    pub async fn run(
        &self,
        cmd: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellResult, DriverError> {
        let started = Instant::now();
        let mut child = Command::new(&self.shell)
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| DriverError::Acceptance(format!("spawn shell: {e}")))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let read_stdout = read_capped(stdout, self.output_byte_limit);
        let read_stderr = read_capped(stderr, self.output_byte_limit);

        let wait = child.wait();
        let race = async {
            tokio::select! {
                w = wait => w,
            }
        };
        let res = tokio::time::timeout(timeout, race).await;

        match res {
            Ok(Ok(status)) => {
                let stdout = read_stdout.await;
                let stderr = read_stderr.await;
                Ok(ShellResult {
                    exit_code: status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                    duration: started.elapsed(),
                })
            }
            Ok(Err(e)) => Err(DriverError::Acceptance(format!("shell wait: {e}"))),
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(self.forced_kill_after, child.wait()).await;
                let stdout = read_stdout.await;
                let stderr = read_stderr.await;
                Ok(ShellResult {
                    exit_code: None,
                    stdout,
                    stderr,
                    timed_out: true,
                    duration: started.elapsed(),
                })
            }
        }
    }
}

async fn read_capped<R>(reader: Option<R>, limit: usize) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut r) = reader else {
        return String::new();
    };
    let mut buf = Vec::with_capacity(limit.min(8192));
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(limit.saturating_sub(buf.len()));
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= limit {
                    // Drain rest to let the child finish writing.
                    let mut sink = [0u8; 8192];
                    while r.read(&mut sink).await.unwrap_or(0) > 0 {}
                    break;
                }
            }
            Err(_) => break,
        }
    }
    // UTF-8 lossy, then truncate at last valid char if cap landed
    // mid-codepoint.
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_exit_zero() {
        let r = ShellRunner::default()
            .run("echo hello", &std::env::temp_dir(), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(r.exit_code, Some(0));
        assert!(r.stdout.contains("hello"));
        assert!(!r.timed_out);
    }

    #[tokio::test]
    async fn false_exits_one() {
        let r = ShellRunner::default()
            .run("false", &std::env::temp_dir(), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(r.exit_code, Some(1));
    }

    #[tokio::test]
    async fn timeout_marks_timed_out() {
        let r = ShellRunner::default()
            .run("sleep 5", &std::env::temp_dir(), Duration::from_millis(100))
            .await
            .unwrap();
        assert!(r.timed_out, "expected timed_out=true");
        assert_eq!(r.exit_code, None);
    }

    #[tokio::test]
    async fn cwd_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        let r = ShellRunner::default()
            .run("pwd", dir.path(), Duration::from_secs(5))
            .await
            .unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert!(r.stdout.contains(&canonical.display().to_string()));
    }
}
