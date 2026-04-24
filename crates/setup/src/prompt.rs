//! Interactive prompts built on top of `dialoguer`.
//!
//! UX contract:
//!
//! * **Main menu** → `MultiSelect` (arrows + space to tick + enter to
//!   confirm). Operators can batch several services in one run.
//! * **Text / list / number fields** → `Input` (line edit + readline).
//! * **Secret fields** → `Password` (echo off).
//! * **Bool fields** → `Confirm` (y/N).
//! * **Choice fields** → `Select` (arrows + enter).
//!
//! Falls back to non-interactive mode (reads stdin line by line) when
//! stdout/stdin are not a TTY so CI and scripted pipes still work.

use std::io::{self, BufRead, IsTerminal};
use std::path::Path;

use anyhow::Result;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect, Password, Select};

use crate::registry::{FieldDef, FieldKind, ServiceDef, ServiceValues};
use crate::status;

fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

/// Top-level service picker.
///
/// Two-step flow:
///
/// 1. Pick one or more categories (LLM, Memory, Plugin, Skill, Infra,
///    Runtime). Only categories that have at least one service are
///    offered.
/// 2. For each selected category, pick services from a MultiSelect
///    narrowed to that category.
///
/// Keeps the flat-list fallback for non-TTY / piped runs so scripts
/// still work via `agent setup <id>`.
pub fn select_services<'a>(
    services: &'a [ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<Vec<&'a ServiceDef>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("(no TTY — use `agent setup <service>` directly)");
        return Ok(Vec::new());
    }

    let report = status::audit(services, secrets_dir, config_dir);

    // Category order + pretty labels. Filter out empty ones so we
    // don't show options the operator can't actually pick.
    use crate::registry::Category;
    let ordered_cats: &[Category] = &[
        Category::Agent,
        Category::Llm,
        Category::Memory,
        Category::Plugin,
        Category::Skill,
        Category::Infra,
        Category::Runtime,
    ];
    let cats: Vec<Category> = ordered_cats
        .iter()
        .copied()
        .filter(|c| services.iter().any(|s| s.category == *c))
        .collect();

    println!();
    println!("╭────────────────────────────────╮");
    println!("│  Agent Framework — Setup       │");
    println!("╰────────────────────────────────╯");
    println!("  ↑/↓ navegar · space seleccionar · enter confirmar · esc back/exit");
    println!();

    let t = theme();
    let mut out_indices: Vec<usize> = Vec::new();

    // Outer loop lets us go back from a category submenu to the top
    // category picker. `esc` on a category submenu breaks out of the
    // inner loop and restarts the outer one.
    loop {
        let cat_labels: Vec<String> = cats
            .iter()
            .map(|c| {
                let total = services.iter().filter(|s| s.category == *c).count();
                let done = services
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.category == *c)
                    .filter(|(i, _)| report.services[*i].is_fully_configured())
                    .count();
                format!("{:<10} · {done}/{total} configurados", c.label())
            })
            .collect();
        let cat_picks = MultiSelect::with_theme(&t)
            .with_prompt("Categorías a revisar (esc = salir)")
            .items(&cat_labels)
            .interact_opt()?;
        let Some(cat_picks) = cat_picks else {
            // Esc on outer picker = user wants to exit setup.
            break;
        };
        if cat_picks.is_empty() {
            break;
        }

        // Walk each selected category. `went_back` flips when the
        // operator hit Esc inside a submenu — we jump straight back
        // to the category picker instead of marching through the
        // remaining categories.
        let mut went_back = false;
        for cat_idx in cat_picks {
            let cat = cats[cat_idx];
            // Build the service list with a synthetic "go back"
            // entry at index 0 so both Esc and ticking it work.
            // Sort within category: fully configured ✔ first, partial
            // · next, missing ✗ last. Operator sees what's done at
            // the top and what needs attention below — natural read.
            let mut svc_in_cat: Vec<(usize, &ServiceDef)> = services
                .iter()
                .enumerate()
                .filter(|(_, s)| s.category == cat)
                .collect();
            svc_in_cat.sort_by_key(|(i, _)| {
                let st = &report.services[*i];
                if st.is_fully_configured() {
                    0
                } else if st.is_partially_configured() {
                    1
                } else {
                    2
                }
            });
            let mut items: Vec<String> = vec!["← Volver a categorías".to_string()];
            for (i, svc) in &svc_in_cat {
                let st = &report.services[*i];
                let marker = if st.is_fully_configured() {
                    "✔"
                } else if st.is_partially_configured() {
                    "·"
                } else {
                    "✗"
                };
                items.push(format!("{marker} {:<42} ({})", svc.label, svc.id));
            }
            println!();
            let picks = MultiSelect::with_theme(&t)
                .with_prompt(format!("▎{} — servicios (esc = volver)", cat.label()))
                .items(&items)
                .interact_opt()?;
            let Some(picks) = picks else {
                // Esc → back to category picker.
                went_back = true;
                break;
            };
            // If the operator ticked the "← Volver" synthetic item,
            // discard any other selections from this submenu and
            // restart the outer loop so they can re-pick categories.
            if picks.contains(&0) {
                went_back = true;
                break;
            }
            for local_idx in picks {
                // Offset by 1 because index 0 is the synthetic row.
                let svc_idx = local_idx - 1;
                out_indices.push(svc_in_cat[svc_idx].0);
            }
        }
        if went_back {
            continue;
        }
        break;
    }

    out_indices.sort_unstable();
    out_indices.dedup();
    Ok(out_indices.into_iter().map(|i| &services[i]).collect())
}

/// Back-compat shim for callers that still want a single-service pick.
/// Uses `Select` (arrows + enter) and returns `None` when the user
/// cancels with Esc/q.
pub fn select_service<'a>(
    services: &'a [ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<Option<&'a ServiceDef>> {
    let picks = select_services(services, secrets_dir, config_dir)?;
    Ok(picks.into_iter().next())
}

/// Run the form for one service. Returns `None` when the user aborts
/// at the confirmation step.
pub fn run_form(
    svc: &ServiceDef,
    _secrets_dir: &Path,
    _config_dir: &Path,
) -> Result<Option<ServiceValues>> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut values = ServiceValues::default();

    for field in &svc.fields {
        let value = if interactive {
            prompt_field(field)?
        } else {
            read_field_non_interactive(field)?
        };
        values.insert(field.key, value);
    }

    if interactive {
        println!();
        println!("Resumen:");
        for (k, v) in values.iter() {
            let field = svc.fields.iter().find(|f| f.key == k);
            let is_secret = matches!(field.map(|f| f.kind), Some(FieldKind::Secret));
            let shown = if is_secret {
                mask(v)
            } else if v.is_empty() {
                "(vacío)".to_string()
            } else {
                v.clone()
            };
            println!("  {k:<20} = {shown}");
        }
        println!();
        let t = theme();
        let confirm = Confirm::with_theme(&t)
            .with_prompt("Confirmar y escribir?")
            .default(true)
            .interact_opt()?;
        if !confirm.unwrap_or(false) {
            return Ok(None);
        }
    }
    Ok(Some(values))
}

fn prompt_field(field: &FieldDef) -> Result<String> {
    println!();
    if let Some(help) = field.help {
        println!("  {}", console::style(help).dim());
    }
    let required_badge = if field.required {
        console::style("*").red().to_string()
    } else {
        String::new()
    };
    let label: String = format!("{}{}", field.label, required_badge);
    let t = theme();

    loop {
        let value = match field.kind {
            FieldKind::Secret => {
                let mut pw = Password::with_theme(&t).with_prompt(label.clone());
                if !field.required {
                    pw = pw.allow_empty_password(true);
                }
                pw.interact()?
            }
            FieldKind::Bool => {
                let default = matches!(field.default, Some("true") | Some("yes") | Some("1"));
                let b = Confirm::with_theme(&t)
                    .with_prompt(label.clone())
                    .default(default)
                    .interact()?;
                if b {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            FieldKind::Choice(opts) => {
                let default_idx = field
                    .default
                    .and_then(|d| opts.iter().position(|o| *o == d))
                    .unwrap_or(0);
                let idx = Select::with_theme(&t)
                    .with_prompt(label.clone())
                    .items(opts)
                    .default(default_idx)
                    .interact()?;
                opts[idx].to_string()
            }
            FieldKind::Number | FieldKind::List | FieldKind::Text => {
                let mut input: Input<String> = Input::with_theme(&t).with_prompt(label.clone());
                if let Some(d) = field.default {
                    input = input.default(d.to_string());
                }
                if !field.required {
                    input = input.allow_empty(true);
                }
                input.interact_text()?
            }
        };

        if field.required && value.trim().is_empty() {
            eprintln!("  {}", console::style("⚠ campo requerido").yellow());
            continue;
        }
        if !value.trim().is_empty() {
            if let Some(v) = field.validator {
                if let Err(msg) = v(&value) {
                    eprintln!("  {}", console::style(format!("⚠ {msg}")).yellow());
                    continue;
                }
            }
        }
        return Ok(value);
    }
}

fn read_field_non_interactive(field: &FieldDef) -> Result<String> {
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        if let Some(d) = field.default {
            return Ok(d.to_string());
        }
    }
    Ok(trimmed)
}

/// Simple yes/no wrapper for the interactive wizard. Non-TTY runs
/// return the `default_yes` argument so scripted callers get a
/// deterministic answer.
/// Read the pasted callback URL. Browsers always put it on one line,
/// so `Input` is fine.
pub fn ask_callback_url() -> anyhow::Result<String> {
    let t = theme();
    let raw: String = Input::with_theme(&t)
        .with_prompt("Callback URL")
        .allow_empty(false)
        .interact_text()?;
    Ok(raw.trim().to_string())
}

/// Single-select prompt over a list of labels. Returns the chosen
/// index. Non-TTY callers get `0`.
pub fn pick_from_list(prompt: &str, items: &[&str]) -> anyhow::Result<usize> {
    if items.is_empty() {
        anyhow::bail!("empty list");
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(0);
    }
    let t = theme();
    let idx = Select::with_theme(&t)
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()?;
    Ok(idx)
}

/// Same as `pick_from_list` but accepts owned Strings so the caller
/// can embed ANSI escapes (status circles, colors).
pub fn pick_from_strings(prompt: &str, items: &[String]) -> anyhow::Result<usize> {
    if items.is_empty() {
        anyhow::bail!("empty list");
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(0);
    }
    let t = theme();
    let idx = Select::with_theme(&t)
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()?;
    Ok(idx)
}

/// Multi-select checkbox prompt. `preselected` is the set of items
/// ticked on entry (already-attached plugins, typically). Returns the
/// full list the user confirmed.
pub fn multi_select(
    prompt: &str,
    items: &[&str],
    preselected: &[bool],
) -> anyhow::Result<Vec<String>> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(Vec::new());
    }
    let t = theme();
    let idxs = MultiSelect::with_theme(&t)
        .with_prompt(prompt)
        .items(items)
        .defaults(preselected)
        .interact()?;
    Ok(idxs.into_iter().map(|i| items[i].to_string()).collect())
}

/// Multi-select variant with owned-string labels (for ANSI colors).
/// The returned list contains the DISPLAY strings; callers that need
/// a stable id should pair labels with ids via an index lookup.
pub fn multi_select_strings(
    prompt: &str,
    items: &[String],
    preselected: &[bool],
) -> anyhow::Result<Vec<usize>> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(Vec::new());
    }
    let t = theme();
    let idxs = MultiSelect::with_theme(&t)
        .with_prompt(prompt)
        .items(items)
        .defaults(preselected)
        .interact()?;
    Ok(idxs)
}

/// Pick one agent id from a list. Non-TTY callers auto-pick the first
/// for deterministic unattended installs; interactive runs always
/// prompt (even for single-agent setups) so the operator sees which
/// agent is about to be touched.
pub fn pick_agent(ids: &[String]) -> anyhow::Result<String> {
    if ids.is_empty() {
        anyhow::bail!("no agents available");
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(ids[0].clone());
    }
    let t = theme();
    let idx = Select::with_theme(&t)
        .with_prompt("¿A qué agente asignar?")
        .items(ids)
        .default(0)
        .interact()?;
    Ok(ids[idx].clone())
}

pub fn yes_no(prompt: &str, default_yes: bool) -> anyhow::Result<bool> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(default_yes);
    }
    let t = theme();
    let answer = Confirm::with_theme(&t)
        .with_prompt(prompt)
        .default(default_yes)
        .interact()?;
    Ok(answer)
}

fn mask(s: &str) -> String {
    if s.is_empty() {
        return "(vacío)".to_string();
    }
    let n = s.chars().count();
    if n <= 4 {
        "•".repeat(n)
    } else {
        format!(
            "{}{}",
            "•".repeat(n - 4),
            s.chars().skip(n - 4).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_hides_most_of_short_input() {
        assert_eq!(mask("abcd"), "••••");
        assert_eq!(mask("abcdef"), "••cdef");
        assert_eq!(mask(""), "(vacío)");
    }
}
