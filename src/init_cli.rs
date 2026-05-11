//! `nexo init` subcommand.
//!
//! Generates the 19 sample YAML files an operator might want to
//! customize, each densely commented with the field semantics +
//! sane defaults already filled in. Templates are embedded at
//! compile time via [`include_str!`] so the binary ships
//! standalone (no template-dir resolution at install time).
//!
//! Filtering by `--yaml <name,name,...>` selects a subset; the
//! special name `plugins` is shorthand for the five
//! `plugins/*.yaml` templates. `--stdout` emits to stdout
//! instead of writing files (useful for `nexo init --stdout >
//! /etc/nexo/broker.yaml`). `--force` overwrites existing
//! files (default behaviour skips them with a `[skip]` log
//! line).

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Embedded (filename, contents) pairs for every YAML the
/// daemon's `AppConfig::load` knows about. Filenames are
/// relative to the config dir; subdirs (`plugins/`,
/// `personas/`) are written when needed.
pub const TEMPLATES: &[(&str, &str)] = &[
    // Required-but-defaulted.
    ("agents.yaml", include_str!("init_templates/agents.yaml")),
    ("broker.yaml", include_str!("init_templates/broker.yaml")),
    ("llm.yaml", include_str!("init_templates/llm.yaml")),
    ("memory.yaml", include_str!("init_templates/memory.yaml")),
    // Top-level optional.
    (
        "extensions.yaml",
        include_str!("init_templates/extensions.yaml"),
    ),
    ("mcp.yaml", include_str!("init_templates/mcp.yaml")),
    (
        "mcp_server.yaml",
        include_str!("init_templates/mcp_server.yaml"),
    ),
    ("runtime.yaml", include_str!("init_templates/runtime.yaml")),
    ("pollers.yaml", include_str!("init_templates/pollers.yaml")),
    (
        "taskflow.yaml",
        include_str!("init_templates/taskflow.yaml"),
    ),
    (
        "transcripts.yaml",
        include_str!("init_templates/transcripts.yaml"),
    ),
    ("pairing.yaml", include_str!("init_templates/pairing.yaml")),
    (
        "webhook_receiver.yaml",
        include_str!("init_templates/webhook_receiver.yaml"),
    ),
    // Plugins subdir.
    (
        "plugins/whatsapp.yaml",
        include_str!("init_templates/plugins/whatsapp.yaml"),
    ),
    (
        "plugins/telegram.yaml",
        include_str!("init_templates/plugins/telegram.yaml"),
    ),
    (
        "plugins/email.yaml",
        include_str!("init_templates/plugins/email.yaml"),
    ),
    (
        "plugins/browser.yaml",
        include_str!("init_templates/plugins/browser.yaml"),
    ),
    (
        "plugins/discovery.yaml",
        include_str!("init_templates/plugins/discovery.yaml"),
    ),
    // Personas subdir.
    (
        "personas/discovery.yaml",
        include_str!("init_templates/personas/discovery.yaml"),
    ),
];

/// Resolve a `--yaml <filter>` token to one or more templates.
/// Accepts: `broker`, `agents`, `plugins`, `plugins/whatsapp`,
/// `whatsapp` (shorthand → `plugins/whatsapp`), etc. Returns
/// the matching `(filename, contents)` pairs.
fn select_templates(filter: Option<&str>) -> Vec<(&'static str, &'static str)> {
    let Some(filter) = filter else {
        return TEMPLATES.to_vec();
    };
    let names: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
    let mut out: Vec<(&'static str, &'static str)> = Vec::new();
    for name in names {
        if name == "plugins" {
            // Special: select every plugins/*.yaml.
            for (path, body) in TEMPLATES {
                if path.starts_with("plugins/") {
                    out.push((*path, *body));
                }
            }
            continue;
        }
        // Try exact path, then `<name>.yaml`, then
        // `plugins/<name>.yaml` as a shorthand.
        let candidates = [
            name.to_string(),
            format!("{}.yaml", name),
            format!("plugins/{}.yaml", name),
            format!("personas/{}.yaml", name),
        ];
        let mut found = false;
        for cand in &candidates {
            if let Some(entry) = TEMPLATES.iter().find(|(p, _)| p == cand) {
                out.push(*entry);
                found = true;
                break;
            }
        }
        if !found {
            eprintln!("  [warn] unknown yaml `{name}` — skipping");
        }
    }
    out
}

/// Run the `nexo init` subcommand.
///
/// - `output_dir`: target dir for the YAML files. Defaults to the
///   `args.config_dir` if `None` — `parse_args` always supplies
///   one (XDG fallback).
/// - `yaml_filter`: comma-list `--yaml broker,llm`. `None` → all
///   19 templates.
/// - `force`: when true, overwrite existing files (default skips).
/// - `stdout`: when true, emit to stdout (one big concatenated
///   stream with `---` separators) instead of writing files.
pub fn run_init(
    output_dir: &Path,
    yaml_filter: Option<&str>,
    force: bool,
    stdout: bool,
) -> Result<()> {
    let selected = select_templates(yaml_filter);
    if selected.is_empty() {
        anyhow::bail!("no templates selected — `--yaml` filter matched nothing");
    }

    if stdout {
        // Concatenated to stdout. Each file prefixed with a
        // comment header so the operator can split later.
        for (filename, body) in &selected {
            println!("# ───── {filename} ─────");
            print!("{body}");
            println!();
        }
        return Ok(());
    }

    // Write to disk.
    std::fs::create_dir_all(output_dir)?;
    let mut written = 0usize;
    let mut skipped = 0usize;
    for (filename, body) in &selected {
        let target = output_dir.join(filename);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if target.exists() && !force {
            println!(
                "  [skip] {} (already exists; pass --force to overwrite)",
                target.display()
            );
            skipped += 1;
            continue;
        }
        std::fs::write(&target, body)?;
        println!("  [write] {}", target.display());
        written += 1;
    }
    println!(
        "✓ wrote {written} template(s), skipped {skipped} (config dir: {})",
        output_dir.display()
    );
    Ok(())
}

/// Helper used by `parse_args` to convert the
/// positional args after `init` into the filter / output / flags
/// triplet.
pub fn parse_init_args(positional: &[String]) -> (Option<String>, Option<PathBuf>, bool, bool) {
    let mut yaml_filter: Option<String> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut force = false;
    let mut stdout = false;
    let mut i = 0;
    while i < positional.len() {
        match positional[i].as_str() {
            "--yaml" => {
                if let Some(v) = positional.get(i + 1) {
                    yaml_filter = Some(v.clone());
                    i += 2;
                    continue;
                }
            }
            "--output" => {
                if let Some(v) = positional.get(i + 1) {
                    output_dir = Some(PathBuf::from(v));
                    i += 2;
                    continue;
                }
            }
            "--force" => force = true,
            "--stdout" => stdout = true,
            _ => {}
        }
        i += 1;
    }
    (yaml_filter, output_dir, force, stdout)
}
