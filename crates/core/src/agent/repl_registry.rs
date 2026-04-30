use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use dashmap::DashMap;
use nexo_config::types::repl::ReplConfig;
use uuid::Uuid;

/// Metadata about a running REPL session.
#[derive(Debug, Clone)]
pub struct ReplSession {
    pub id: String,
    pub runtime: String,
    pub cwd: String,
    pub spawned_at: chrono::DateTime<Utc>,
    pub output_len: u64,
    pub exit_code: Option<i32>,
}

/// Output from an exec or read action.
#[derive(Debug, Clone)]
pub struct ReplOutput {
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
}

struct ReplProcess {
    session: ReplSession,
    child: StdMutex<Child>,
    stdin: StdMutex<ChildStdin>,
    stdout_buf: StdMutex<Vec<u8>>,
    stderr_buf: StdMutex<Vec<u8>>,
    #[allow(dead_code)]
    max_output_bytes: u64,
}

/// Registry of persistent REPL sessions. One per agent — stored in
/// `AgentContext`. Feature-gated behind `repl-tool`.
pub struct ReplRegistry {
    sessions: DashMap<String, Arc<ReplProcess>>,
    config: ReplConfig,
    workspace_dir: String,
}

impl ReplRegistry {
    pub fn new(config: ReplConfig, workspace_dir: String) -> Self {
        Self {
            sessions: DashMap::new(),
            config,
            workspace_dir,
        }
    }

    /// Spawn a new REPL session. Returns the session ID.
    pub async fn spawn(&self, runtime: &str, cwd: Option<&str>) -> Result<String> {
        if !self.config.allowed_runtimes.iter().any(|r| r == runtime) {
            bail!(
                "runtime '{}' not in allowed list: {:?}",
                runtime,
                self.config.allowed_runtimes
            );
        }
        if self.sessions.len() >= self.config.max_sessions as usize {
            bail!(
                "session limit reached ({}) — kill an existing session first",
                self.config.max_sessions
            );
        }

        let cwd = cwd.unwrap_or(&self.workspace_dir).to_string();

        let (program, args) = match runtime {
            "python" => ("python3", vec!["-u", "-q", "-i"]),
            "node" => ("node", vec!["-i", "--no-warnings"]),
            "bash" => ("bash", vec!["--norc", "--noprofile"]),
            other => bail!("unknown runtime: {}", other),
        };

        let mut child = Command::new(program)
            .args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("failed to spawn {} REPL: {}", runtime, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin handle"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout handle"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("no stderr handle"))?;

        let session_id = Uuid::new_v4().to_string();
        let session = ReplSession {
            id: session_id.clone(),
            runtime: runtime.to_string(),
            cwd,
            spawned_at: Utc::now(),
            output_len: 0,
            exit_code: None,
        };

        let proc = Arc::new(ReplProcess {
            session,
            child: StdMutex::new(child),
            stdin: StdMutex::new(stdin),
            stdout_buf: StdMutex::new(Vec::new()),
            stderr_buf: StdMutex::new(Vec::new()),
            max_output_bytes: self.config.max_output_bytes,
        });

        // Background reader threads (blocking I/O, one per stream).
        let cap = self.config.max_output_bytes;

        {
            let bg = proc.clone();
            std::thread::spawn(move || Self::read_stream::<_>(bg, stdout, cap, true));
        }
        {
            let bg = proc.clone();
            std::thread::spawn(move || Self::read_stream::<_>(bg, stderr, cap, false));
        }

        // Brief yield so the OS has time to start the child process.
        tokio::task::yield_now().await;

        self.sessions.insert(session_id.clone(), proc.clone());

        // Let the background reader threads consume startup output
        // (Node welcome banner, Python `>>> ` prompt, etc.) so the
        // first exec snapshot captures a clean baseline.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let _ = proc.read_buffers();

        Ok(session_id)
    }

    /// Blocking helper: read from a stream until EOF, appending to the
    /// corresponding buffer (capped).
    fn read_stream<R: std::io::Read>(
        proc: Arc<ReplProcess>,
        mut stream: R,
        cap: u64,
        is_stdout: bool,
    ) {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = if is_stdout {
                        proc.stdout_buf.lock().unwrap()
                    } else {
                        proc.stderr_buf.lock().unwrap()
                    };
                    guard.extend_from_slice(&buf[..n]);
                    let cap_usize = cap as usize;
                    if guard.len() > cap_usize {
                        let excess = guard.len() - cap_usize;
                        guard.drain(..excess);
                    }
                }
            }
        }
    }

    /// Execute code in a running session and return the output.
    pub async fn exec(&self, session_id: &str, code: &str) -> Result<ReplOutput> {
        let proc = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

        proc.check_alive()?;

        // Snapshot current buffer state.
        let (out_before, err_before) = proc.read_buffers();

        // Write code to stdin (blocking write is fine for short payloads).
        {
            let mut stdin = proc.stdin.lock().unwrap();
            stdin
                .write_all(code.as_bytes())
                .map_err(|e| anyhow!("write to REPL stdin failed: {}", e))?;
            stdin
                .write_all(b"\n")
                .map_err(|e| anyhow!("write newline to REPL stdin failed: {}", e))?;
            stdin
                .flush()
                .map_err(|e| anyhow!("flush REPL stdin failed: {}", e))?;
        }

        // Wait with timeout for new output.
        let deadline = tokio::time::Duration::from_secs(self.config.timeout_secs as u64);
        let result = tokio::time::timeout(
            deadline,
            proc.wait_for_new_output(out_before.len(), err_before.len()),
        )
        .await;

        match result {
            Ok(output) => Ok(output),
            Err(_elapsed) => {
                let (stdout, stderr) = proc.read_buffers();
                Ok(ReplOutput {
                    stdout: String::from_utf8_lossy(diff(&stdout, out_before.len())).into_owned(),
                    stderr: String::from_utf8_lossy(diff(&stderr, err_before.len())).into_owned(),
                    timed_out: true,
                    exit_code: None,
                })
            }
        }
    }

    /// Read current output buffers without sending code.
    pub async fn read(&self, session_id: &str) -> Result<ReplOutput> {
        let proc = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

        proc.check_alive()?;

        let (stdout, stderr) = proc.read_buffers();
        Ok(ReplOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            timed_out: false,
            exit_code: None,
        })
    }

    /// Kill a session by ID.
    pub async fn kill(&self, session_id: &str) -> Result<()> {
        let (_id, proc) = self
            .sessions
            .remove(session_id)
            .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

        let mut child = proc.child.lock().unwrap();
        child
            .kill()
            .map_err(|e| anyhow!("kill failed for session {}: {}", session_id, e))?;
        Ok(())
    }

    /// List all active sessions.
    pub fn list(&self) -> Vec<ReplSession> {
        self.sessions
            .iter()
            .map(|entry| entry.value().session.clone())
            .collect()
    }
}

impl ReplProcess {
    fn check_alive(&self) -> Result<()> {
        let mut child = self.child.lock().unwrap();
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                bail!(
                    "session {} terminated (exit code {})",
                    self.session.id,
                    code.map_or(-1, |c| c)
                );
            }
            Ok(None) => Ok(()),
            Err(e) => Err(anyhow!("process check failed: {}", e)),
        }
    }

    fn read_buffers(&self) -> (Vec<u8>, Vec<u8>) {
        let out = self.stdout_buf.lock().unwrap().clone();
        let err = self.stderr_buf.lock().unwrap().clone();
        (out, err)
    }

    /// Wait for NEW output after an exec. Polls every 50ms until buffer
    /// grows beyond the pre-exec snapshot or the process dies.
    async fn wait_for_new_output(
        &self,
        out_before_len: usize,
        err_before_len: usize,
    ) -> ReplOutput {
        loop {
            {
                let mut child = self.child.lock().unwrap();
                if let Ok(Some(status)) = child.try_wait() {
                    let (stdout, stderr) = self.read_buffers();
                    return ReplOutput {
                        stdout: String::from_utf8_lossy(diff(&stdout, out_before_len))
                            .into_owned(),
                        stderr: String::from_utf8_lossy(diff(&stderr, err_before_len))
                            .into_owned(),
                        timed_out: false,
                        exit_code: status.code(),
                    };
                }
            }

            let (stdout, stderr) = self.read_buffers();
            if stdout.len() > out_before_len || stderr.len() > err_before_len {
                return ReplOutput {
                    stdout: String::from_utf8_lossy(diff(&stdout, out_before_len)).into_owned(),
                    stderr: String::from_utf8_lossy(diff(&stderr, err_before_len)).into_owned(),
                    timed_out: false,
                    exit_code: None,
                };
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }
}

/// Return a slice of new bytes since the snapshot, handling cap truncation.
fn diff(data: &[u8], before_len: usize) -> &[u8] {
    if data.len() >= before_len {
        &data[before_len..]
    } else {
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn has_runtime(name: &str) -> bool {
        match name {
            "python" => {
                StdCommand::new("python3")
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
            "node" => StdCommand::new("node")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            "bash" => StdCommand::new("bash")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            _ => false,
        }
    }

    fn test_config() -> ReplConfig {
        ReplConfig {
            enabled: true,
            allowed_runtimes: vec!["python".into(), "node".into(), "bash".into()],
            max_sessions: 3,
            timeout_secs: 5,
            max_output_bytes: 65536,
        }
    }

    #[tokio::test]
    async fn test_spawn_python_hello_world() {
        if !has_runtime("python") {
            eprintln!("SKIP: python3 not found");
            return;
        }
        let reg = ReplRegistry::new(test_config(), "/tmp".into());
        let sid = reg.spawn("python", None).await.expect("spawn python");
        let out = reg
            .exec(&sid, "print('hello from repl')")
            .await
            .expect("exec");
        assert!(out.stdout.contains("hello from repl"), "got: {:?}", out);
        reg.kill(&sid).await.ok();
    }

    #[tokio::test]
    async fn test_spawn_node_eval() {
        if !has_runtime("node") {
            eprintln!("SKIP: node not found");
            return;
        }
        let reg = ReplRegistry::new(test_config(), "/tmp".into());
        let sid = reg.spawn("node", None).await.expect("spawn node");
        let out = reg
            .exec(&sid, "console.log(1+1)")
            .await
            .expect("exec");
        assert!(out.stdout.contains("2"), "got: {:?}", out);
        reg.kill(&sid).await.ok();
    }

    #[tokio::test]
    async fn test_spawn_bash_echo() {
        if !has_runtime("bash") {
            eprintln!("SKIP: bash not found");
            return;
        }
        let reg = ReplRegistry::new(test_config(), "/tmp".into());
        let sid = reg.spawn("bash", None).await.expect("spawn bash");
        let out = reg.exec(&sid, "echo okbash").await.expect("exec");
        assert!(out.stdout.contains("okbash"), "got: {}", out.stdout);
        reg.kill(&sid).await.ok();
    }

    #[tokio::test]
    async fn test_spawn_rejects_disallowed_runtime() {
        let mut cfg = test_config();
        cfg.allowed_runtimes = vec!["python".into()];
        let reg = ReplRegistry::new(cfg, "/tmp".into());
        let err = reg.spawn("node", None).await.unwrap_err();
        assert!(
            err.to_string().contains("not in allowed list"),
            "got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_session_limit() {
        if !has_runtime("bash") {
            eprintln!("SKIP: bash not found");
            return;
        }
        let mut cfg = test_config();
        cfg.max_sessions = 2;
        let reg = ReplRegistry::new(cfg, "/tmp".into());
        let s1 = reg.spawn("bash", None).await.expect("spawn 1");
        let s2 = reg.spawn("bash", None).await.expect("spawn 2");
        let err = reg.spawn("bash", None).await.unwrap_err();
        assert!(
            err.to_string().contains("session limit"),
            "got: {}",
            err
        );
        reg.kill(&s1).await.ok();
        reg.kill(&s2).await.ok();
    }

    #[tokio::test]
    async fn test_kill_session_then_exec_fails() {
        if !has_runtime("bash") {
            eprintln!("SKIP: bash not found");
            return;
        }
        let reg = ReplRegistry::new(test_config(), "/tmp".into());
        let sid = reg.spawn("bash", None).await.expect("spawn");
        reg.kill(&sid).await.expect("kill");
        let err = reg.exec(&sid, "echo nope").await.unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'session not found', got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_list_sessions() {
        if !has_runtime("bash") {
            eprintln!("SKIP: bash not found");
            return;
        }
        let reg = ReplRegistry::new(test_config(), "/tmp".into());
        let s1 = reg.spawn("bash", None).await.expect("spawn 1");
        let s2 = reg.spawn("bash", None).await.expect("spawn 2");
        let list = reg.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|s| s.id == s1));
        assert!(list.iter().any(|s| s.id == s2));
        reg.kill(&s1).await.ok();
        reg.kill(&s2).await.ok();
    }

    #[tokio::test]
    async fn test_timeout_returns_partial_output() {
        if !has_runtime("python") {
            eprintln!("SKIP: python3 not found");
            return;
        }
        let mut cfg = test_config();
        cfg.timeout_secs = 2;
        let reg = ReplRegistry::new(cfg, "/tmp".into());
        let sid = reg.spawn("python", None).await.expect("spawn python");
        // First exec runs fast (no sleep) so the snapshot baseline is past
        // startup output.
        let _ = reg.exec(&sid, "x = 1").await.expect("preamble exec");
        // Second exec sleeps longer than timeout — should time out.
        let out = reg
            .exec(&sid, "import time; time.sleep(10)")
            .await
            .expect("exec");
        assert!(out.timed_out, "expected timeout, got: {:?}", out);
        reg.kill(&sid).await.ok();
    }

    #[tokio::test]
    async fn test_output_buffer_cap_truncates() {
        if !has_runtime("python") {
            eprintln!("SKIP: python3 not found");
            return;
        }
        let mut cfg = test_config();
        cfg.max_output_bytes = 100;
        let reg = ReplRegistry::new(cfg, "/tmp".into());
        let sid = reg.spawn("python", None).await.expect("spawn python");
        let out = reg
            .exec(&sid, "print('x' * 500)")
            .await
            .expect("exec");
        let len = out.stdout.len();
        assert!(
            len <= 200,
            "output should be capped, got {} bytes",
            len
        );
        reg.kill(&sid).await.ok();
    }
}
