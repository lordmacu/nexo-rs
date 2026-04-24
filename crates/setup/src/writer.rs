//! Atomic, perms-aware persistence for secrets + YAML patches.
//!
//! Secrets land under `secrets/<file>` with mode `0600`. We write to a
//! sibling tempfile inside the same directory and `rename` into place so
//! a crash mid-write never leaves a half-file on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result};

use crate::registry::{FieldDef, FieldTarget, ServiceDef, ServiceValues};
use crate::yaml_patch;

/// Guarantee the secrets directory exists with `0700` mode on unix.
pub fn ensure_secrets_dir(secrets_dir: &Path) -> Result<()> {
    fs::create_dir_all(secrets_dir)
        .with_context(|| format!("mkdir {}", secrets_dir.display()))?;
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(secrets_dir)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(secrets_dir, perms)
            .with_context(|| format!("chmod 0700 {}", secrets_dir.display()))?;
    }
    Ok(())
}

/// Write a value to `secrets/<file>` atomically with mode `0600`.
///
/// If an existing secret lives at the target path, it is copied to
/// `<target>.bak` before the new one lands — re-running setup can't
/// silently destroy a working key. Only one generation is kept
/// (`.bak` is overwritten on subsequent rotations) so the directory
/// stays bounded.
///
/// Skipped entirely if `value` is empty — we don't want a file full of
/// nothing to shadow a real env-var override.
pub fn write_secret(secrets_dir: &Path, file: &str, value: &str) -> Result<PathBuf> {
    let target = secrets_dir.join(file);
    if value.is_empty() {
        if target.exists() {
            fs::remove_file(&target)
                .with_context(|| format!("remove {}", target.display()))?;
        }
        return Ok(target);
    }
    ensure_secrets_dir(secrets_dir)?;

    // Tempfile created with mode `0600` up front — no TOCTOU window
    // where the secret is world-readable between creation and chmod.
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        builder.permissions(Permissions::from_mode(0o600));
    }
    let tmp = builder
        .tempfile_in(secrets_dir)
        .with_context(|| format!("tempfile in {}", secrets_dir.display()))?;
    {
        let mut file = tmp.reopen()?;
        file.write_all(value.as_bytes())?;
        file.flush()?;
        file.sync_all().ok();
    }
    tmp.as_file().sync_all().ok();

    // Back up any existing secret before clobbering it. `.bak` is a
    // single generation — good enough to let the operator recover
    // from a typo without building up an unbounded history.
    if target.exists() {
        let backup = target.with_extension(
            target
                .extension()
                .map(|e| format!("{}.bak", e.to_string_lossy()))
                .unwrap_or_else(|| "bak".to_string()),
        );
        fs::copy(&target, &backup).with_context(|| {
            format!("backup {} → {}", target.display(), backup.display())
        })?;
        tracing::info!(backup = %backup.display(), "previous secret backed up");
    }

    tmp.persist(&target)
        .map_err(|e| anyhow::anyhow!("persist secret {}: {e}", target.display()))?;
    tracing::info!(path = %target.display(), "secret written");
    Ok(target)
}

/// Persist everything the user typed for one service.
pub fn persist(
    svc: &ServiceDef,
    values: &ServiceValues,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    // MiniMax is a special case: the destination secret file depends on
    // the `key_kind` the operator picked (Coding Plan vs API key), and
    // we also patch `config/llm.yaml::providers.minimax.base_url` based
    // on the region. We take over the whole persist path instead of
    // letting the default field-by-field loop handle it.
    if svc.id == "minimax" {
        return persist_minimax(values, secrets_dir, config_dir);
    }

    for field in &svc.fields {
        let Some(value) = values.get(field.key) else { continue };
        if value.is_empty() && !field.required {
            continue;
        }
        persist_field(field, value, secrets_dir, config_dir)?;
    }
    Ok(())
}

fn persist_minimax(
    values: &ServiceValues,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let key_kind = values.get("key_kind").unwrap_or("plan").trim().to_string();
    let key_value = values
        .get("key_value")
        .ok_or_else(|| anyhow::anyhow!("missing key_value"))?
        .trim()
        .to_string();
    if key_value.is_empty() {
        anyhow::bail!("key_value cannot be empty");
    }
    let group_id = values
        .get("group_id")
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if group_id.is_empty() {
        anyhow::bail!("group_id cannot be empty");
    }
    let region = values.get("region").unwrap_or("global").trim().to_string();

    let secret_file = match key_kind.as_str() {
        "plan" => "minimax_code_plan_key.txt",
        "api" => "minimax_api_key.txt",
        other => anyhow::bail!("unknown key_kind `{other}`"),
    };
    write_secret(secrets_dir, secret_file, &key_value)?;
    write_secret(secrets_dir, "minimax_group_id.txt", &group_id)?;

    // Token Plan keys (`sk-cp-…`) speak the Anthropic Messages wire
    // over `{host}/anthropic/v1/messages`; regular API keys stay on
    // the public OpenAI-compat endpoint. This is the critical
    // compatibility gate — point the client at the wrong path and
    // MiniMax returns 401/404.
    let (base_url, api_flavor) = match (key_kind.as_str(), region.as_str()) {
        ("plan", "cn") => ("https://api.minimaxi.com/anthropic", "anthropic_messages"),
        ("plan", "chat") => ("https://api.minimax.io/anthropic", "anthropic_messages"),
        ("plan", _) => ("https://api.minimax.io/anthropic", "anthropic_messages"),
        (_, "cn") => ("https://api.minimaxi.com/v1", "openai_compat"),
        (_, "chat") => ("https://api.minimax.chat/v1", "openai_compat"),
        _ => ("https://api.minimax.io/v1", "openai_compat"),
    };
    let yaml_path = config_dir.join("llm.yaml");
    crate::yaml_patch::upsert(
        &yaml_path,
        "providers.minimax.base_url",
        base_url,
        crate::yaml_patch::ValueKind::String,
    )?;
    crate::yaml_patch::upsert(
        &yaml_path,
        "providers.minimax.api_flavor",
        api_flavor,
        crate::yaml_patch::ValueKind::String,
    )?;

    println!();
    println!("✔ MiniMax configurado:");
    println!("  Tipo       : {key_kind} → secrets/{secret_file}");
    println!("  Región     : {region}");
    println!("  Base URL   : {base_url}");
    println!("  API flavor : {api_flavor}");
    println!();
    println!("Siguiente paso: exporta el key al entorno (o apunta config/llm.yaml");
    println!("a `\"${{file:{}}}\"`):", secrets_dir.join(secret_file).display());
    if key_kind == "plan" {
        println!("  export MINIMAX_CODE_PLAN_KEY=$(cat {})",
            secrets_dir.join(secret_file).display());
    } else {
        println!("  export MINIMAX_API_KEY=$(cat {})",
            secrets_dir.join(secret_file).display());
    }
    println!("  export MINIMAX_GROUP_ID=$(cat {})",
        secrets_dir.join("minimax_group_id.txt").display());
    Ok(())
}

#[allow(dead_code)]
fn persist_minimax_portal(values: &ServiceValues, secrets_dir: &Path) -> Result<()> {
    let region_url = match values.get("region").unwrap_or("global") {
        "cn" => "https://api.minimaxi.com",
        _ => "https://api.minimax.io",
    };
    let access_token = values
        .get("access_token")
        .ok_or_else(|| anyhow::anyhow!("missing access_token"))?
        .trim()
        .to_string();
    if access_token.is_empty() {
        anyhow::bail!("access_token cannot be empty");
    }
    let refresh_token = values
        .get("refresh_token")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    let ttl: i64 = values
        .get("expires_in_secs")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(86_400);
    let now = chrono::Utc::now().timestamp();
    let expires_at = now + ttl;

    // Consolidated bundle — this is the ONLY file the LLM client needs.
    // We still persist the three sidecar files for operators who want
    // to `cat` individual pieces or source them into a shell.
    let bundle = serde_json::json!({
        "access_token":  access_token,
        "refresh_token": refresh_token,
        "expires_at":    expires_at,
        "region":        region_url,
        "obtained_at":   chrono::Utc::now().to_rfc3339(),
    });
    write_secret(
        secrets_dir,
        "minimax_portal.json",
        &serde_json::to_string_pretty(&bundle)?,
    )?;
    write_secret(secrets_dir, "minimax_portal_access.txt", &access_token)?;
    write_secret(secrets_dir, "minimax_portal_refresh.txt", &refresh_token)?;
    write_secret(
        secrets_dir,
        "minimax_portal_expires.txt",
        &expires_at.to_string(),
    )?;

    println!();
    println!("✔ Token Plan guardado.");
    println!("  Región    : {region_url}");
    println!("  Expira en : {} h", ttl / 3600);
    if refresh_token.is_empty() {
        println!();
        println!("⚠  Sin refresh_token → cuando el access expire, vuelve a correr");
        println!("   `agent setup minimax-portal` para pegar uno nuevo.");
    } else {
        println!();
        println!("✔ Refresh token presente → el cliente se auto-refresca.");
    }
    println!();
    println!("El cliente `crates/llm/src/minimax.rs` detecta");
    println!("`{}` y usa este token directamente.", secrets_dir.join("minimax_portal.json").display());
    Ok(())
}

fn persist_field(
    field: &FieldDef,
    value: &str,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    match &field.target {
        FieldTarget::Secret { file, env_var: _ } => {
            write_secret(secrets_dir, file, value)?;
        }
        FieldTarget::Yaml { file, path } => {
            let yaml_path = config_dir.join(file);
            yaml_patch::upsert(&yaml_path, path, value, field_to_yaml_type(field))?;
        }
        FieldTarget::EnvOnly(_) => {
            // Env-only fields: nothing to persist on disk. The caller
            // prints a summary at the end so the operator can pipe it
            // into their shell rc / systemd EnvFile.
        }
    }
    Ok(())
}

fn field_to_yaml_type(field: &FieldDef) -> yaml_patch::ValueKind {
    use crate::registry::FieldKind;
    match field.kind {
        FieldKind::Bool => yaml_patch::ValueKind::Bool,
        FieldKind::Number => yaml_patch::ValueKind::Number,
        FieldKind::List => yaml_patch::ValueKind::List,
        _ => yaml_patch::ValueKind::String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_secret_creates_0600_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        let path = write_secret(dir, "k.txt", "hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        #[cfg(unix)]
        {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn empty_value_removes_existing_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        let path = write_secret(dir, "k.txt", "hello").unwrap();
        assert!(path.exists());
        write_secret(dir, "k.txt", "").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn write_secret_is_atomic_across_rewrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        write_secret(dir, "k.txt", "v1").unwrap();
        write_secret(dir, "k.txt", "v2").unwrap();
        assert_eq!(fs::read_to_string(dir.join("k.txt")).unwrap(), "v2");
    }

    #[test]
    fn overwrite_backs_up_previous_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        write_secret(dir, "k.txt", "first").unwrap();
        write_secret(dir, "k.txt", "second").unwrap();
        assert_eq!(fs::read_to_string(dir.join("k.txt")).unwrap(), "second");
        assert_eq!(fs::read_to_string(dir.join("k.txt.bak")).unwrap(), "first");
    }
}
