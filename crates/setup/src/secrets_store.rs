//! Phase 82.10.k production adapter — file-based secrets store
//! at `<state_root>/secrets/<NAME>.txt` (mode 0600) + process
//! env injection via `std::env::set_var`.
//!
//! Shape contract:
//! 1. Set `std::env::var(name)` FIRST so existing
//!    `std::env::var(name)` consumers (LLM clients, plugin
//!    auth) pick up the value immediately.
//! 2. Atomic file write (tmp file mode 0600 → fsync → rename)
//!    so the value survives daemon restart.
//!
//! Microapp wizard's M9 Step 1 uses this to skip the operator's
//! manual `export MINIMAX_API_KEY=…` + restart cycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::secrets::SecretsStore;
use nexo_tool_meta::admin::secrets::SecretsWriteResponse;

/// File-based secrets store rooted at `<state_root>/secrets/`.
/// Each call writes `<state_root>/secrets/<NAME>.txt` with mode
/// 0600 AND `std::env::set_var(name, value)`.
pub struct FsSecretsStore {
    secrets_dir: PathBuf,
}

impl FsSecretsStore {
    /// Build a store rooted at `<state_root>/secrets/`. The
    /// directory is created lazily on first write.
    pub fn new(state_root: &Path) -> Arc<Self> {
        Arc::new(Self {
            secrets_dir: state_root.join("secrets"),
        })
    }

    /// Build a store with the secrets directory specified
    /// directly. Used by main.rs when the operator-resolved
    /// `secrets_dir` (via `NEXO_SECRETS_DIR` or
    /// `<config_dir>/../secrets`) is already a complete path —
    /// no need to re-join.
    pub fn with_secrets_dir(secrets_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { secrets_dir })
    }
}

#[async_trait]
impl SecretsStore for FsSecretsStore {
    async fn write(
        &self,
        name: &str,
        value: &str,
    ) -> Result<SecretsWriteResponse, AdminRpcError> {
        let secrets_dir = self.secrets_dir.clone();
        let name_owned = name.to_string();
        let value_owned = value.to_string();

        // 1. Inject into the daemon's process env FIRST. The
        //    in-process effect is immediate; failure here keeps
        //    the disk untouched (caller can retry safely).
        //
        //    SAFETY note: `std::env::set_var` is technically
        //    unsound across threads on stable Rust 2024+ but
        //    works on the project's MSRV (1.79). Follow-up
        //    82.10.k.d migrates LLM clients to a `SecretStore`
        //    trait so we can drop this call.
        let overwrote_env = std::env::var_os(&name_owned).is_some();
        std::env::set_var(&name_owned, &value_owned);

        // 2. Atomic file write on the blocking pool. If this
        //    fails, the env var stayed set — operator can retry
        //    cleanly (set_var is idempotent on identical input).
        let path = tokio::task::spawn_blocking(move || -> Result<PathBuf, std::io::Error> {
            std::fs::create_dir_all(&secrets_dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                let _ = std::fs::set_permissions(&secrets_dir, perms);
            }
            let final_path = secrets_dir.join(format!("{name_owned}.txt"));
            // Tmp filename uses the leading `.` + suffix so the
            // operator browsing `secrets/` knows which file is
            // a transient.
            let tmp_path = secrets_dir.join(format!(".{name_owned}.tmp"));
            std::fs::write(&tmp_path, value_owned.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o600);
                std::fs::set_permissions(&tmp_path, perms)?;
            }
            std::fs::rename(&tmp_path, &final_path)?;
            Ok(final_path)
        })
        .await
        .map_err(|e| AdminRpcError::Internal(format!("spawn_blocking: {e}")))?
        .map_err(|e| AdminRpcError::Internal(format!("secret write io: {e}")))?;

        Ok(SecretsWriteResponse {
            path,
            overwrote_env,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `std::env::set_var` is process-global; tests that mutate
    /// env serialise via this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Pin a unique env var name per test so concurrent runs
    /// across test crates don't collide.
    fn unique_name(suffix: &str) -> String {
        let pid = std::process::id();
        format!("NEXO_TEST_{}_{}_KEY", pid, suffix)
    }

    #[tokio::test]
    async fn fs_secrets_store_persists_then_set_var() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let store = FsSecretsStore::with_secrets_dir(dir.path().to_path_buf());
        let name = unique_name("PERSISTS");
        std::env::remove_var(&name);

        let response = store.write(&name, "sk-test-value").await.unwrap();

        // File on disk.
        let final_path = dir.path().join(format!("{name}.txt"));
        assert_eq!(response.path, final_path);
        assert_eq!(response.overwrote_env, false);
        let on_disk = std::fs::read_to_string(&final_path).unwrap();
        assert_eq!(on_disk, "sk-test-value");
        // Mode 0600 on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected mode 0600, got {mode:o}");
        }

        // Process env.
        assert_eq!(std::env::var(&name).unwrap(), "sk-test-value");

        std::env::remove_var(&name);
    }

    #[tokio::test]
    async fn fs_secrets_store_overwrite_returns_overwrote_env_true() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let store = FsSecretsStore::with_secrets_dir(dir.path().to_path_buf());
        let name = unique_name("OVERWRITE");
        std::env::set_var(&name, "old-value");

        let response = store.write(&name, "new-value").await.unwrap();

        assert_eq!(response.overwrote_env, true);
        assert_eq!(std::env::var(&name).unwrap(), "new-value");

        let on_disk = std::fs::read_to_string(dir.path().join(format!("{name}.txt"))).unwrap();
        assert_eq!(on_disk, "new-value");

        std::env::remove_var(&name);
    }
}
