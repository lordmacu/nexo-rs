//! `CustomVerifier` trait + registry + the two built-ins shipped in
//! 67.5: `no_paths_touched` and `git_clean`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Value;

use crate::acceptance::shell::ShellRunner;
use crate::error::DriverError;

#[async_trait]
pub trait CustomVerifier: Send + Sync + 'static {
    /// `Ok(None)` ⇒ pass; `Ok(Some(message))` ⇒ fail; `Err` ⇒
    /// infrastructure problem (propagated up).
    async fn verify(&self, args: &Value, workspace: &Path) -> Result<Option<String>, DriverError>;
}

#[derive(Default)]
pub struct CustomVerifierRegistry {
    inner: DashMap<String, Arc<dyn CustomVerifier>>,
}

impl CustomVerifierRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, name: impl Into<String>, v: Arc<dyn CustomVerifier>) {
        self.inner.insert(name.into(), v);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn CustomVerifier>> {
        self.inner.get(name).map(|e| e.value().clone())
    }

    /// Register the two 67.5 built-ins under their canonical names.
    pub fn with_builtins(self) -> Self {
        let np: Arc<dyn CustomVerifier> = Arc::new(NoPathsTouched::default());
        let gc: Arc<dyn CustomVerifier> = Arc::new(GitClean::default());
        self.register("no_paths_touched", np);
        self.register("git_clean", gc);
        self
    }
}

/// Fails when any path Claude touched (per `git diff --name-only HEAD`)
/// starts with one of the configured `prefixes`.
///
/// `args`: `{"prefixes": ["secrets/", "private/"]}`. Empty prefixes
/// → pass. Workspace not a git repo → pass (verifier irrelevant).
#[derive(Default)]
pub struct NoPathsTouched {
    pub shell: ShellRunner,
}

#[async_trait]
impl CustomVerifier for NoPathsTouched {
    async fn verify(&self, args: &Value, workspace: &Path) -> Result<Option<String>, DriverError> {
        let prefixes: Vec<String> = args
            .get("prefixes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if prefixes.is_empty() {
            return Ok(None);
        }
        if !workspace.join(".git").exists() {
            return Ok(None);
        }
        let res = self
            .shell
            .run(
                "git diff --name-only HEAD",
                workspace,
                Duration::from_secs(30),
            )
            .await?;
        if res.timed_out || res.exit_code != Some(0) {
            return Ok(Some(format!(
                "no_paths_touched: git diff failed (exit {:?})",
                res.exit_code
            )));
        }
        let mut hits = Vec::new();
        for line in res.stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if prefixes.iter().any(|p| line.starts_with(p)) {
                hits.push(line.to_string());
            }
        }
        if hits.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!(
                "no_paths_touched: modified prefixed paths: {}",
                hits.join(", ")
            )))
        }
    }
}

/// Fails when `git status --porcelain` returns any output. Workspace
/// not a git repo → pass.
#[derive(Default)]
pub struct GitClean {
    pub shell: ShellRunner,
}

#[async_trait]
impl CustomVerifier for GitClean {
    async fn verify(&self, _args: &Value, workspace: &Path) -> Result<Option<String>, DriverError> {
        if !workspace.join(".git").exists() {
            return Ok(None);
        }
        let res = self
            .shell
            .run("git status --porcelain", workspace, Duration::from_secs(30))
            .await?;
        if res.timed_out || res.exit_code != Some(0) {
            return Ok(Some(format!(
                "git_clean: git status failed (exit {:?})",
                res.exit_code
            )));
        }
        if res.stdout.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!(
                "git_clean: workspace dirty:\n{}",
                res.stdout.trim()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    pub struct ScriptedVerifier {
        outcomes: Mutex<std::collections::VecDeque<Option<String>>>,
    }

    impl ScriptedVerifier {
        pub fn new<I: IntoIterator<Item = Option<String>>>(items: I) -> Self {
            Self {
                outcomes: Mutex::new(items.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl CustomVerifier for ScriptedVerifier {
        async fn verify(
            &self,
            _args: &Value,
            _workspace: &Path,
        ) -> Result<Option<String>, DriverError> {
            Ok(self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Some("scripted exhausted".into())))
        }
    }

    #[tokio::test]
    async fn registry_register_and_get() {
        let r = CustomVerifierRegistry::new();
        let s: Arc<dyn CustomVerifier> = Arc::new(ScriptedVerifier::new([None]));
        r.register("scripted", s);
        assert!(r.get("scripted").is_some());
        assert!(r.get("missing").is_none());
    }

    #[tokio::test]
    async fn scripted_returns_in_order() {
        let v = ScriptedVerifier::new([None, Some("nope".into())]);
        let dir = tempfile::tempdir().unwrap();
        let r1 = v.verify(&Value::Null, dir.path()).await.unwrap();
        let r2 = v.verify(&Value::Null, dir.path()).await.unwrap();
        assert!(r1.is_none());
        assert_eq!(r2.as_deref(), Some("nope"));
    }

    #[tokio::test]
    async fn no_paths_touched_no_git_passes() {
        // No .git → verifier irrelevant.
        let dir = tempfile::tempdir().unwrap();
        let v = NoPathsTouched::default();
        let args = serde_json::json!({"prefixes": ["secrets/"]});
        assert!(v.verify(&args, dir.path()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn git_clean_no_git_passes() {
        let dir = tempfile::tempdir().unwrap();
        let v = GitClean::default();
        assert!(v.verify(&Value::Null, dir.path()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn with_builtins_registers_two_names() {
        let r = CustomVerifierRegistry::default().with_builtins();
        assert!(r.get("no_paths_touched").is_some());
        assert!(r.get("git_clean").is_some());
    }
}
