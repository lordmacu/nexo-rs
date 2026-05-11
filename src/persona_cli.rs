//! CLI surface for the
//! `nexo persona <subcommand>` family. Sister of
//! `src/plugin_install.rs` + `src/plugin_admin.rs` but for
//! v2 persona packs (out-of-tree agent definitions).
//!
//! Subcommands implemented:
//! - `nexo persona install <coords>` — install pipeline:
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
/// [--target <triple>] [--json]` — runs the install orchestrator
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
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"personas": []})).unwrap()
            );
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
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"personas": rows})).unwrap()
        );
    } else if rows.is_empty() {
        println!(
            "(no personas found in {} search path(s))",
            cfg.personas.discovery.search_paths.len()
        );
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

/// `nexo persona get <id> [--json]` — surface the full
/// manifest + computed contributes paths for one persona.
/// JSON variant emits the manifest section verbatim plus
/// resolved absolute paths so CI can grep specific fields.
pub async fn run_persona_get(config_dir: &Path, id: String, json: bool) -> Result<i32> {
    let cfg = AppConfig::load(config_dir).context("load config")?;
    if cfg.personas.discovery.search_paths.is_empty() {
        return Ok(emit_string_error(
            "no personas.discovery.search_paths configured; nothing to look up",
            json,
        ));
    }
    let discovered =
        nexo_persona_installer::discover_personas(&cfg.personas.discovery.search_paths).await;
    let target = match discovered.into_iter().find(|d| d.manifest.persona.id == id) {
        Some(t) => t,
        None => {
            return Ok(emit_string_error(
                &format!("persona `{id}` not installed under any configured search path"),
                json,
            ))
        }
    };

    let agent_paths: Vec<String> = target
        .agent_config_paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let plugin_partial_paths: Vec<String> = target
        .plugin_config_partial_paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    if json {
        let payload = json!({
            "ok": true,
            "id": target.manifest.persona.id,
            "version": target.manifest.persona.version,
            "description": target.manifest.persona.description,
            "min_nexo_version": target.manifest.persona.min_nexo_version,
            "homepage": target.manifest.persona.homepage,
            "install_root": target.install_root.display().to_string(),
            "agent_config_paths": agent_paths,
            "plugin_config_partial_paths": plugin_partial_paths,
            "requires": target.manifest.persona.requires.as_ref().map(|r| json!({
                "plugins": r.plugins,
                "features": r.features,
                "env_vars": r.env_vars.iter().map(|e| json!({
                    "name": e.name,
                    "required": e.required,
                    "description": e.description,
                })).collect::<Vec<_>>(),
            })),
            "meta": target.manifest.persona.meta.as_ref().map(|m| json!({
                "author": m.author,
                "license": m.license,
                "repository": m.repository,
            })),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("id:               {}", target.manifest.persona.id);
        println!("version:          {}", target.manifest.persona.version);
        println!("description:      {}", target.manifest.persona.description);
        println!(
            "min_nexo_version: {}",
            target.manifest.persona.min_nexo_version
        );
        if let Some(h) = &target.manifest.persona.homepage {
            println!("homepage:         {h}");
        }
        println!("install_root:     {}", target.install_root.display());
        if !agent_paths.is_empty() {
            println!("agent_configs:");
            for p in &agent_paths {
                println!("  - {p}");
            }
        }
        if !plugin_partial_paths.is_empty() {
            println!("plugin_configs_partial:");
            for p in &plugin_partial_paths {
                println!("  - {p}");
            }
        }
        if let Some(req) = &target.manifest.persona.requires {
            if !req.plugins.is_empty() {
                println!("requires.plugins: {}", req.plugins.join(", "));
            }
            if !req.features.is_empty() {
                println!("requires.features: {}", req.features.join(", "));
            }
            if !req.env_vars.is_empty() {
                println!("requires.env_vars:");
                for e in &req.env_vars {
                    let req_marker = if e.required {
                        "*required*"
                    } else {
                        "optional "
                    };
                    println!("  - {req_marker} {} — {}", e.name, e.description);
                }
            }
        }
    }
    Ok(0)
}

/// `nexo persona upgrade <id> [--json]` — re-resolve the
/// installed persona's source repo (extracted from
/// `manifest.persona.homepage`) at `latest`, run the install
/// pipeline. Refuses to downgrade — if the resolved version
/// is older than what's installed, prints a diagnostic +
/// exits non-zero. Idempotent on same-version.
pub async fn run_persona_upgrade(config_dir: &Path, id: String, json: bool) -> Result<i32> {
    let cfg = AppConfig::load(config_dir).context("load config")?;
    if cfg.personas.discovery.search_paths.is_empty() {
        return Ok(emit_string_error(
            "no personas.discovery.search_paths configured; install with \
             `nexo persona install <coords>` first",
            json,
        ));
    }
    let discovered =
        nexo_persona_installer::discover_personas(&cfg.personas.discovery.search_paths).await;
    let installed = match discovered.iter().find(|d| d.manifest.persona.id == id) {
        Some(d) => d.clone(),
        None => {
            return Ok(emit_string_error(
                &format!("persona `{id}` is not installed; nothing to upgrade"),
                json,
            ))
        }
    };

    // Need the source repo to know what to re-resolve. The
    // manifest's homepage field is the only on-disk clue.
    let homepage = match installed.manifest.persona.homepage.as_deref() {
        Some(h) => h,
        None => {
            return Ok(emit_string_error(
                &format!(
                    "persona `{id}` manifest has no `homepage` — cannot infer source repo for \
                     re-resolve. Run `nexo persona install <owner>/<repo>` manually."
                ),
                json,
            ))
        }
    };
    let coords_str = match super::github_owner_repo_from_url(homepage) {
        Some((owner, repo)) => format!("{owner}/{repo}"),
        None => {
            return Ok(emit_string_error(
                &format!(
                    "persona `{id}` homepage `{homepage}` is not a recognizable GitHub URL — \
                     cannot infer source repo. Run `nexo persona install <owner>/<repo>` manually."
                ),
                json,
            ))
        }
    };

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

    // Pre-flight downgrade check: peek the resolved version
    // before paying for the full download. Reuse the
    // contract resolver directly.
    let client = reqwest::Client::new();
    let resolved = match nexo_ext_installer::resolve_release_with_contract(
        &nexo_persona_installer::PersonaExtractContract,
        &client,
        &coords,
        &nexo_ext_installer::current_target_triple(),
        DEFAULT_GITHUB_API_BASE,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(emit_install_error(
                &PersonaInstallError::Ext(e),
                json,
                Some(&coords_str),
            ))
        }
    };
    let installed_version = match installed
        .manifest
        .persona
        .version
        .parse::<semver::Version>()
    {
        Ok(v) => v,
        Err(_) => semver::Version::new(0, 0, 0),
    };
    if resolved.version < installed_version {
        return Ok(emit_string_error(
            &format!(
                "refusing to downgrade `{id}` from {installed_version} to {} — pin a newer tag \
                 with `nexo persona install {coords_str}@<tag>` if intentional",
                resolved.version
            ),
            json,
        ));
    }
    if resolved.version == installed_version {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "id": id,
                    "version": installed_version.to_string(),
                    "no_op": true,
                }))
                .unwrap()
            );
        } else {
            println!("persona `{id}` already at latest ({installed_version}) — no upgrade needed");
        }
        return Ok(0);
    }

    // Delegate to the install pipeline — same install_root as
    // currently-installed (parent of install_root).
    let install_root = installed
        .install_root
        .parent()
        .unwrap_or(&installed.install_root)
        .to_path_buf();
    let result = install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: &nexo_ext_installer::current_target_triple(),
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
                    "from_version": installed_version.to_string(),
                    "to_version": installed.version.to_string(),
                    "install_root": installed.install_root.display().to_string(),
                });
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            } else {
                println!(
                    "upgraded `{}` from {} to {} at {}",
                    installed.id,
                    installed_version,
                    installed.version,
                    installed.install_root.display()
                );
            }
            Ok(0)
        }
        Err(e) => Ok(emit_install_error(&e, json, Some(&coords_str))),
    }
}

/// Inner-loop dev override. Mirrors `plugin_run::PluginRunOverride`:
/// the dispatch handler stamps this on `args.persona_run_override`
/// then falls through to daemon boot, which applies it to the
/// loaded `AppConfig`.
#[derive(Debug, Clone)]
pub struct PersonaRunOverride {
    /// Absolute parent dir of the persona pack — what gets
    /// prepended to `cfg.personas.discovery.search_paths`.
    pub search_path_inject: PathBuf,
    /// Absolute path of the persona pack dir
    /// (`<parent>/<id>-<version>/`).
    pub persona_root: PathBuf,
    /// Absolute path of the on-disk `persona.toml`.
    pub manifest_path: PathBuf,
    /// Persona id from the manifest.
    pub persona_id: String,
    /// Persona version (string-encoded).
    pub persona_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PersonaRunError {
    #[error("path `{}` does not exist", path.display())]
    PathNotFound { path: PathBuf },
    #[error(
        "path `{}` is neither a directory containing persona.toml nor a manifest file",
        path.display()
    )]
    NotAPersonaPath { path: PathBuf },
    #[error("manifest at `{}` failed to parse / validate: {reason}", path.display())]
    ManifestInvalid { path: PathBuf, reason: String },
    #[error(
        "persona path `{}` has no parent dir — cannot derive a search_path injection target",
        path.display()
    )]
    OrphanedPath { path: PathBuf },
    #[error("io error: {0}")]
    Io(String),
}

/// Resolve `<path>` to a persona-run override. Either a dir
/// containing `persona.toml` (typical inner-loop layout) or
/// the manifest file itself. No filesystem mutation; the
/// dispatch handler applies the returned override to the
/// loaded `AppConfig` before daemon boot.
pub fn resolve_local_persona(
    raw_path: &Path,
) -> std::result::Result<PersonaRunOverride, PersonaRunError> {
    if !raw_path.exists() {
        return Err(PersonaRunError::PathNotFound {
            path: raw_path.to_path_buf(),
        });
    }
    let abs = std::fs::canonicalize(raw_path)
        .map_err(|e| PersonaRunError::Io(format!("canonicalize {}: {e}", raw_path.display())))?;
    let (persona_root, manifest_path) = if abs.is_file() {
        if abs.file_name().and_then(|n| n.to_str()) != Some("persona.toml") {
            return Err(PersonaRunError::NotAPersonaPath { path: abs });
        }
        let parent = abs
            .parent()
            .ok_or_else(|| PersonaRunError::NotAPersonaPath { path: abs.clone() })?
            .to_path_buf();
        (parent, abs)
    } else if abs.is_dir() {
        let candidate = abs.join("persona.toml");
        if !candidate.is_file() {
            return Err(PersonaRunError::NotAPersonaPath { path: abs });
        }
        (abs, candidate)
    } else {
        return Err(PersonaRunError::NotAPersonaPath { path: abs });
    };

    let body =
        std::fs::read_to_string(&manifest_path).map_err(|e| PersonaRunError::ManifestInvalid {
            path: manifest_path.clone(),
            reason: format!("read failed: {e}"),
        })?;
    let manifest =
        nexo_persona_manifest::parse_str(&body).map_err(|e| PersonaRunError::ManifestInvalid {
            path: manifest_path.clone(),
            reason: e.to_string(),
        })?;
    nexo_persona_manifest::validate(&manifest).map_err(|e| PersonaRunError::ManifestInvalid {
        path: manifest_path.clone(),
        reason: e.to_string(),
    })?;

    let search_path_inject = persona_root
        .parent()
        .ok_or_else(|| PersonaRunError::OrphanedPath {
            path: persona_root.clone(),
        })?
        .to_path_buf();

    Ok(PersonaRunOverride {
        search_path_inject,
        persona_root,
        manifest_path,
        persona_id: manifest.persona.id.clone(),
        persona_version: manifest.persona.version.clone(),
    })
}

/// Apply the override to a loaded `AppConfig` — prepends
/// `search_path_inject` to `cfg.personas.discovery.search_paths`
/// (idempotent).
pub fn apply_persona_run_override(cfg: &mut AppConfig, override_: &PersonaRunOverride) {
    let already_at_head = cfg
        .personas
        .discovery
        .search_paths
        .first()
        .map(|p| p == &override_.search_path_inject)
        .unwrap_or(false);
    if !already_at_head {
        cfg.personas
            .discovery
            .search_paths
            .insert(0, override_.search_path_inject.clone());
    }
}

/// Pre-boot banner for persona run. JSON or human-mode.
pub fn print_persona_run_banner(override_: &PersonaRunOverride, json: bool) {
    if json {
        let payload = json!({
            "ok": true,
            "id": override_.persona_id,
            "version": override_.persona_version,
            "manifest_path": override_.manifest_path.display().to_string(),
            "persona_root": override_.persona_root.display().to_string(),
            "search_path_inject": override_.search_path_inject.display().to_string(),
            "next": "daemon-boot",
        });
        println!("{}", serde_json::to_string(&payload).unwrap_or_default());
        return;
    }
    eprintln!(
        "→ Resolving local persona at {}",
        override_.persona_root.display()
    );
    eprintln!(
        "✓ Manifest valid: {}@{}",
        override_.persona_id, override_.persona_version
    );
    eprintln!(
        "✓ Booting daemon with {} prepended to personas.discovery.search_paths",
        override_.search_path_inject.display()
    );
}

/// Render a persona-run resolve error in human or JSON mode.
pub fn emit_persona_run_error(err: &PersonaRunError, json: bool, path: Option<PathBuf>) -> i32 {
    if json {
        let payload = json!({
            "ok": false,
            "error": err.to_string(),
            "path": path.as_ref().map(|p| p.display().to_string()),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        eprintln!("error: {err}");
    }
    1
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

  get <id>                        Print the full manifest + computed
                                  contributes paths for <id>.
                                  Flags: --json
  upgrade <id>                    Re-resolve the installed persona's
                                  source repo at latest + install if
                                  newer. Refuses to downgrade.
                                  Flags: --json
  run <path>                      Inner-loop dev: validate a local
                                  persona pack + boot the daemon with
                                  its parent dir prepended to
                                  personas.discovery.search_paths.
                                  Mirror of `nexo plugin run`.
                                  Flags: --json
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
