//! Phase F6 of `cody-cli-install` — CLI surface for the
//! `nexo persona <subcommand>` family. Sister of
//! `src/plugin_install.rs` + `src/plugin_admin.rs` but for
//! v2 persona packs (out-of-tree agent definitions).
//!
//! Subcommands implemented:
//! - `nexo persona install <coords>` — F1+F3 pipeline:
//!   resolve → validate → download → verify → extract.
//! - `nexo persona list` — scan
//!   `cfg.personas.discovery.search_paths` and tabulate.
//! - `nexo persona remove <id>` — atomic dir removal.
//! - `nexo persona help` — static help text.
//!
//! Stubbed (return non-zero exit + actionable hint pointing
//! at the deferred follow-up):
//! - `nexo persona get <id>` — operator can `cat
//!   <install_root>/persona.toml` meanwhile.
//! - `nexo persona upgrade <id>` — operator can run
//!   `nexo persona install <coords>` with a newer tag.
//! - `nexo persona run <path>` — inner-loop dev (mirror of
//!   `nexo plugin run`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nexo_config::AppConfig;
use nexo_ext_installer::{RepoCoords, DEFAULT_GITHUB_API_BASE};
use nexo_persona_installer::{install_persona, InstallInputs, PersonaInstallError};
use serde_json::json;

/// `nexo persona install <owner>/<repo>[@<tag>] [--dest <dir>]
/// [--target <triple>] [--json]` — runs the F3 orchestrator
/// against a real GitHub Releases endpoint. Resolves
/// `--dest` (or
/// `cfg.personas.discovery.search_paths[0]` /
/// `<state_dir>/personas/`) as the install root.
pub async fn run_persona_install(
    config_dir: &Path,
    coords_str: String,
    dest_override: Option<PathBuf>,
    target_override: Option<String>,
    json: bool,
) -> Result<i32> {
    let cfg = AppConfig::load(config_dir).context("load config")?;

    let coords = match RepoCoords::parse(&coords_str) {
        Ok(c) => c,
        Err(e) => {
            return Ok(emit_install_error(
                &PersonaInstallError::Ext(e),
                json,
                Some(&coords_str),
            ))
        }
    };

    let install_root = resolve_install_root(&cfg, dest_override)?;
    let target = target_override.unwrap_or_else(nexo_ext_installer::current_target_triple);

    let client = reqwest::Client::new();
    let result = install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: &target,
        install_root: &install_root,
        api_base: DEFAULT_GITHUB_API_BASE,
    })
    .await;

    match result {
        Ok(installed) => {
            if json {
                let payload = json!({
                    "ok": true,
                    "id": installed.id,
                    "version": installed.version.to_string(),
                    "install_root": installed.install_root.display().to_string(),
                    "tarball_bytes": installed.tarball_bytes,
                    "was_already_present": installed.was_already_present,
                });
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            } else if installed.was_already_present {
                println!(
                    "persona `{}` v{} already installed at {}",
                    installed.id,
                    installed.version,
                    installed.install_root.display()
                );
            } else {
                println!(
                    "installed persona `{}` v{} at {} ({} bytes)",
                    installed.id,
                    installed.version,
                    installed.install_root.display(),
                    installed.tarball_bytes
                );
            }
            Ok(0)
        }
        Err(e) => Ok(emit_install_error(&e, json, Some(&coords_str))),
    }
}

/// `nexo persona list [--json]` — scan
/// `cfg.personas.discovery.search_paths` and render every
/// installed persona. Honors disabled / allowlist filters.
pub async fn run_persona_list(config_dir: &Path, json: bool) -> Result<i32> {
    let cfg = AppConfig::load(config_dir).context("load config")?;
    if cfg.personas.discovery.search_paths.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&json!({"personas": []})).unwrap());
        } else {
            eprintln!(
                "no personas.discovery.search_paths configured; nothing to list. \
                 add a search path under <config_dir>/personas/discovery.yaml or \
                 install via `nexo persona install <owner>/<repo>` to seed."
            );
        }
        return Ok(0);
    }

    let discovered =
        nexo_persona_installer::discover_personas(&cfg.personas.discovery.search_paths).await;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for d in discovered {
        let id = d.manifest.persona.id.clone();
        if !cfg.personas.discovery.id_passes_filters(&id) {
            continue;
        }
        rows.push(json!({
            "id": id,
            "version": d.manifest.persona.version,
            "install_root": d.install_root.display().to_string(),
            "description": d.manifest.persona.description,
            "homepage": d.manifest.persona.homepage,
        }));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&json!({"personas": rows})).unwrap());
    } else if rows.is_empty() {
        println!("(no personas found in {} search path(s))",
            cfg.personas.discovery.search_paths.len());
    } else {
        for r in &rows {
            println!(
                "{:<20} {:<10} {}",
                r["id"].as_str().unwrap_or(""),
                r["version"].as_str().unwrap_or(""),
                r["install_root"].as_str().unwrap_or(""),
            );
        }
    }
    Ok(0)
}

/// `nexo persona remove <id> [--yes] [--json]` — atomic
/// removal of `<install_root>/<id>-<version>/`. Without
/// `--yes`, prints the resolved path + asks the operator to
/// re-run with the flag.
pub async fn run_persona_remove(
    config_dir: &Path,
    id: String,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let cfg = AppConfig::load(config_dir).context("load config")?;
    if cfg.personas.discovery.search_paths.is_empty() {
        return Ok(emit_string_error(
            "no personas.discovery.search_paths configured; nothing to remove",
            json,
        ));
    }
    let discovered =
        nexo_persona_installer::discover_personas(&cfg.personas.discovery.search_paths).await;
    let target = discovered.into_iter().find(|d| d.manifest.persona.id == id);
    let target = match target {
        Some(t) => t,
        None => {
            return Ok(emit_string_error(
                &format!("persona `{id}` not installed under any configured search path"),
                json,
            ))
        }
    };

    if !yes {
        if json {
            let payload = json!({
                "ok": false,
                "would_remove": target.install_root.display().to_string(),
                "rerun_with": "--yes",
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        } else {
            println!(
                "would remove `{}` at {} — re-run with --yes to confirm",
                id,
                target.install_root.display()
            );
        }
        return Ok(0);
    }

    if let Err(e) = tokio::fs::remove_dir_all(&target.install_root).await {
        return Ok(emit_string_error(
            &format!("remove `{}`: {e}", target.install_root.display()),
            json,
        ));
    }
    if json {
        let payload = json!({
            "ok": true,
            "id": id,
            "removed_root": target.install_root.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("removed `{}` from {}", id, target.install_root.display());
    }
    Ok(0)
}

/// `nexo persona get <id> [--json]` — STUB. Tracked at
/// FOLLOWUPS.md `cody-cli.F6.b`.
pub async fn run_persona_get_stub(_id: String, json: bool) -> Result<i32> {
    Ok(emit_string_error(
        "`nexo persona get` not implemented yet — meanwhile, `nexo persona list` plus \
         `cat <install_root>/persona.toml` covers the same surface. \
         Tracked: FOLLOWUPS.md cody-cli.F6.b",
        json,
    ))
}

/// `nexo persona upgrade <id> [--json]` — STUB. Tracked at
/// FOLLOWUPS.md `cody-cli.F6.b`.
pub async fn run_persona_upgrade_stub(_id: String, json: bool) -> Result<i32> {
    Ok(emit_string_error(
        "`nexo persona upgrade` not implemented yet — meanwhile, run \
         `nexo persona install <owner>/<repo>@<newer-tag>`; the orchestrator \
         is idempotent, so the same-version case is a no-op. \
         Tracked: FOLLOWUPS.md cody-cli.F6.b",
        json,
    ))
}

/// `nexo persona run <path> [--json]` — STUB. Tracked at
/// FOLLOWUPS.md `cody-cli.F6.b`.
pub async fn run_persona_run_stub(_path: PathBuf, json: bool) -> Result<i32> {
    Ok(emit_string_error(
        "`nexo persona run` (inner-loop dev) not implemented yet — the F1+F3 \
         pipeline supports daemon-managed installs only. \
         Tracked: FOLLOWUPS.md cody-cli.F6.b",
        json,
    ))
}

/// Static help text for `nexo persona help`. Mirrors the
/// shape of `nexo plugin help`'s output so operators
/// familiar with one immediately read the other.
pub fn print_persona_help() {
    println!(
        r#"nexo persona <subcommand> — manage v2 persona packs

Personas bundle agent definitions (system prompt + plugin
bindings + workspace seed + secrets templates) installed
out-of-tree from GitHub Releases. Distinct from plugins
(plugins register CODE; personas register CONFIG).

Subcommands:
  install <owner>/<repo>[@<tag>]  Download + verify + extract a
                                  persona pack into the first
                                  configured search path.
                                  Flags: --dest <dir>
                                         --target <triple>
                                         --json
  list                            Tabulate every installed persona
                                  across all configured search paths.
                                  Flags: --json
  remove <id> [--yes]             Remove the install dir for <id>.
                                  Without --yes, prints the path it
                                  WOULD remove + exits 0.
                                  Flags: --json

  get <id>                        STUB — `cat <install_root>/persona.toml`
                                  meanwhile.
  upgrade <id>                    STUB — re-run `install` with newer tag.
  run <path>                      STUB — inner-loop dev (deferred).
  help                            Print this help.

Search paths come from `<config_dir>/personas/discovery.yaml`:

  discovery:
    search_paths:
      - /var/lib/nexo/personas
    disabled: []     # ids to skip even when found
    allowlist: []    # empty = accept any; non-empty = whitelist

Distinct from `install.sh`: v1 packs use install.sh (airgapped /
inner-loop), v2 packs use `nexo persona install` (daemon-managed).
Both flavors coexist; pick per-pack via manifest_version."#
    );
}

// ──────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────

fn resolve_install_root(cfg: &AppConfig, dest_override: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(d) = dest_override {
        return absolutize(d);
    }
    if let Some(first) = cfg.personas.discovery.search_paths.first() {
        return absolutize(first.clone());
    }
    let state = nexo_project_tracker::state::nexo_state_dir();
    Ok(state.join("personas"))
}

fn absolutize(p: PathBuf) -> Result<PathBuf> {
    if p.is_absolute() {
        return Ok(p);
    }
    let cwd = std::env::current_dir().context("read cwd to absolutize install root")?;
    Ok(cwd.join(p))
}

fn emit_install_error(err: &PersonaInstallError, json: bool, coords: Option<&str>) -> i32 {
    if json {
        let payload = json!({
            "ok": false,
            "coords": coords,
            "error": err.to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        eprintln!("error: {err}");
    }
    1
}

fn emit_string_error(msg: &str, json: bool) -> i32 {
    if json {
        let payload = json!({"ok": false, "error": msg});
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        eprintln!("error: {msg}");
    }
    1
}
