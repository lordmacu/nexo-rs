//! Command entry points. Each `run_*` is pure over its `CliContext`.

use std::path::{Path, PathBuf};

use crate::discovery::{DiagnosticLevel, ExtensionDiscovery};
use crate::manifest::{ExtensionManifest, Transport, RESERVED_IDS};

use super::format::{
    render_info_json, render_info_plain, render_list_json, render_list_table, CapabilitiesOut,
    InfoOut, ListRow, CLI_JSON_SCHEMA_VERSION,
};
use super::status::resolve_status_for_candidate;
use super::yaml_edit::{load_or_default, write_atomic};
use super::{extensions_yaml_path, CliContext, CliError};

// ─── list ─────────────────────────────────────────────────────────────────────

pub fn run_list(ctx: CliContext<'_>, json: bool) -> Result<(), CliError> {
    let rows = gather_rows(&ctx);
    if json {
        render_list_json(&rows, ctx.out)?;
    } else {
        render_list_table(&rows, ctx.out)?;
    }
    Ok(())
}

// ─── info ─────────────────────────────────────────────────────────────────────

pub fn run_info(ctx: CliContext<'_>, id: &str, json: bool) -> Result<(), CliError> {
    let discovery = build_discovery(&ctx.extensions);
    let report = discovery.discover();
    let candidate = report
        .candidates
        .into_iter()
        .find(|c| c.manifest.id() == id)
        .ok_or_else(|| CliError::NotFound(id.to_string()))?;
    let status = resolve_status_for_candidate(id, &ctx.extensions.disabled);

    let transport = match candidate.manifest.transport {
        Transport::Stdio { .. } => "stdio",
        Transport::Nats { .. } => "nats",
        Transport::Http { .. } => "http",
    };
    let path_s = candidate.root_dir.display().to_string();

    let mcp_servers_json = if candidate.manifest.mcp_servers.is_empty() {
        None
    } else {
        serde_json::to_value(&candidate.manifest.mcp_servers).ok()
    };

    let info = InfoOut {
        schema_version: CLI_JSON_SCHEMA_VERSION,
        id: &candidate.manifest.plugin.id,
        version: &candidate.manifest.plugin.version,
        name: candidate.manifest.plugin.name.as_deref(),
        description: candidate.manifest.plugin.description.as_deref(),
        min_agent_version: candidate.manifest.plugin.min_agent_version.as_deref(),
        status: status.as_str(),
        transport,
        capabilities: CapabilitiesOut {
            tools: &candidate.manifest.capabilities.tools,
            hooks: &candidate.manifest.capabilities.hooks,
            channels: &candidate.manifest.capabilities.channels,
            providers: &candidate.manifest.capabilities.providers,
        },
        path: &path_s,
        author: candidate.manifest.meta.author.as_deref(),
        license: candidate.manifest.meta.license.as_deref(),
        mcp_servers: mcp_servers_json,
    };

    if json {
        render_info_json(&info, ctx.out)?;
    } else {
        render_info_plain(&info, ctx.out)?;
    }
    Ok(())
}

// ─── validate ─────────────────────────────────────────────────────────────────

pub fn run_validate(ctx: CliContext<'_>, path: &Path) -> Result<(), CliError> {
    let manifest_path = if path.is_dir() {
        path.join(crate::manifest::MANIFEST_FILENAME)
    } else {
        path.to_path_buf()
    };
    match ExtensionManifest::from_path(&manifest_path) {
        Ok(m) => {
            writeln!(
                ctx.out,
                "ok: {} {} ({} tools, {} hooks)",
                m.plugin.id,
                m.plugin.version,
                m.capabilities.tools.len(),
                m.capabilities.hooks.len(),
            )?;
            Ok(())
        }
        Err(e) => Err(CliError::InvalidManifest(format!(
            "{}: {e}",
            manifest_path.display()
        ))),
    }
}

// ─── doctor ───────────────────────────────────────────────────────────────────

pub fn run_doctor(ctx: CliContext<'_>) -> Result<(), CliError> {
    let discovery = build_discovery(&ctx.extensions);
    let report = discovery.discover();

    writeln!(ctx.out, "scanned {} dirs", report.scanned_dirs)?;
    writeln!(ctx.out, "discovered {} candidates", report.candidates.len())?;

    let (errors, warns): (Vec<_>, Vec<_>) = report
        .diagnostics
        .iter()
        .partition(|d| d.level == DiagnosticLevel::Error);

    if errors.is_empty() && warns.is_empty() {
        writeln!(ctx.out, "no diagnostics.")?;
        return Ok(());
    }
    if !errors.is_empty() {
        writeln!(ctx.out, "\nERRORS ({}):", errors.len())?;
        for d in &errors {
            writeln!(ctx.out, "  {}: {}", d.path.display(), d.message)?;
        }
    }
    if !warns.is_empty() {
        writeln!(ctx.out, "\nWARNINGS ({}):", warns.len())?;
        for d in &warns {
            writeln!(ctx.out, "  {}: {}", d.path.display(), d.message)?;
        }
    }
    Ok(())
}

// ─── enable / disable ─────────────────────────────────────────────────────────

pub fn run_enable(ctx: CliContext<'_>, id: &str) -> Result<(), CliError> {
    validate_id(id)?;
    ensure_id_discovered(&ctx, id)?;
    let path = extensions_yaml_path(&ctx.config_dir);
    let mut cfg = load_or_default(&path)?;
    warn_if_divergent_state(&ctx.extensions.disabled, &cfg.disabled);
    let before = cfg.disabled.len();
    cfg.disabled.retain(|x| x != id);
    cfg.disabled.sort();
    cfg.disabled.dedup();
    if cfg.disabled.len() == before {
        writeln!(ctx.out, "already enabled: {id}")?;
        return Ok(());
    }
    write_atomic(&path, &cfg)?;
    writeln!(ctx.out, "enabled: {id}")?;
    Ok(())
}

pub fn run_disable(ctx: CliContext<'_>, id: &str) -> Result<(), CliError> {
    validate_id(id)?;
    ensure_id_discovered(&ctx, id)?;
    let path = extensions_yaml_path(&ctx.config_dir);
    let mut cfg = load_or_default(&path)?;
    warn_if_divergent_state(&ctx.extensions.disabled, &cfg.disabled);
    if cfg.disabled.iter().any(|x| x == id) {
        writeln!(ctx.out, "already disabled: {id}")?;
        return Ok(());
    }
    cfg.disabled.push(id.to_string());
    cfg.disabled.sort();
    cfg.disabled.dedup();
    write_atomic(&path, &cfg)?;
    writeln!(ctx.out, "disabled: {id}")?;
    Ok(())
}

// ─── helpers ──────────────────────────────────────────────────────────────────

/// CLI discovery differs from runtime discovery: we must still see
/// disabled extensions so `list`/`info`/`enable` can address them. The
/// disabled filter is applied only when rendering status.
fn build_discovery(cfg: &agent_config::ExtensionsConfig) -> ExtensionDiscovery {
    let search_paths: Vec<PathBuf> = cfg.search_paths.iter().map(PathBuf::from).collect();
    ExtensionDiscovery::new(
        search_paths,
        cfg.ignore_dirs.clone(),
        Vec::new(),
        cfg.allowlist.clone(),
        cfg.max_depth,
    )
    .with_follow_links(cfg.follow_links)
}

fn gather_rows(ctx: &CliContext<'_>) -> Vec<ListRow> {
    let discovery = build_discovery(&ctx.extensions);
    let report = discovery.discover();

    let mut rows: Vec<ListRow> = report
        .candidates
        .iter()
        .map(|c| {
            let status = resolve_status_for_candidate(&c.manifest.plugin.id, &ctx.extensions.disabled);
            ListRow::from_manifest(&c.manifest, &status, c.root_dir.display().to_string())
        })
        .collect();

    for d in &report.diagnostics {
        if d.level != DiagnosticLevel::Error {
            continue;
        }
        let already = rows
            .iter()
            .any(|r| d.path.starts_with(&r.path) || r.path.starts_with(d.path.display().to_string().as_str()));
        if already {
            continue;
        }
        let id = d
            .path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        rows.push(ListRow::error_row(
            id,
            d.path.display().to_string(),
            d.message.clone(),
        ));
    }

    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

fn validate_id(id: &str) -> Result<(), CliError> {
    if id.is_empty() {
        return Err(CliError::InvalidId(id.into(), "empty".into()));
    }
    if RESERVED_IDS.iter().any(|r| *r == id) {
        return Err(CliError::InvalidId(id.into(), "reserved native id".into()));
    }
    Ok(())
}

fn ensure_id_discovered(ctx: &CliContext<'_>, id: &str) -> Result<(), CliError> {
    let discovery = build_discovery(&ctx.extensions);
    let report = discovery.discover();
    if !report.candidates.iter().any(|c| c.manifest.id() == id) {
        return Err(CliError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Emit a `tracing::warn!` when the `disabled[]` set loaded in
/// `ctx.extensions` (the in-memory view shared with the running agent)
/// doesn't match what's currently on disk — happens when the operator
/// edited `extensions.yaml` by hand between commands. `enable`/`disable`
/// still operate on the on-disk state; this warn just surfaces the
/// divergence so the operator doesn't silently stomp their manual
/// edits.
fn warn_if_divergent_state(in_memory: &[String], on_disk: &[String]) {
    use std::collections::BTreeSet;
    let in_mem: BTreeSet<&str> = in_memory.iter().map(String::as_str).collect();
    let disk: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    if in_mem == disk {
        return;
    }
    let added: Vec<&str> = disk.difference(&in_mem).copied().collect();
    let removed: Vec<&str> = in_mem.difference(&disk).copied().collect();
    tracing::warn!(
        yaml_added = ?added,
        yaml_removed = ?removed,
        "extensions.yaml on disk differs from the in-memory view — command operates on the on-disk copy",
    );
}

#[cfg(test)]
mod divergence_tests {
    use super::warn_if_divergent_state;

    #[test]
    fn no_divergence_is_silent() {
        // Just exercising the happy path — `tracing::warn!` is global
        // so we can't easily capture output here, but the function must
        // not panic on equal inputs.
        warn_if_divergent_state(&["a".into(), "b".into()], &["a".into(), "b".into()]);
    }

    #[test]
    fn divergent_sets_do_not_panic() {
        warn_if_divergent_state(&["a".into()], &["b".into()]);
        warn_if_divergent_state(&[], &["x".into()]);
        warn_if_divergent_state(&["x".into()], &[]);
    }
}
