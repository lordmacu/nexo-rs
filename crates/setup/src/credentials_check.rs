//! Phase 17 — run the credential gauntlet from inside the wizard.
//!
//! The wizard finishes by calling [`run`] which loads the just-written
//! config, runs `nexo_auth::build_credentials` in lenient mode, and
//! pretty-prints the report. Errors do not abort the wizard — the
//! user can always re-run `agent --check-config` later — but they are
//! surfaced immediately so the user catches missing `credentials`
//! bindings before the daemon starts.

use std::path::Path;

use anyhow::Result;

pub struct Summary {
    pub accounts_wa: usize,
    pub accounts_tg: usize,
    pub accounts_google: usize,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub bindings: Vec<(String, Vec<(String, String)>)>,
}

pub fn run(config_dir: &Path) -> Result<Summary> {
    use nexo_auth::CredentialStore;

    let cfg = nexo_config::AppConfig::load(config_dir)?;
    let google = nexo_auth::load_google_auth(config_dir)?;
    let result = nexo_auth::build_credentials(
        &cfg.agents.agents,
        &cfg.plugins.whatsapp,
        &cfg.plugins.telegram,
        &google,
        nexo_auth::StrictLevel::Lenient,
    );

    match result {
        Ok(bundle) => {
            let mut bindings: Vec<(String, Vec<(String, String)>)> = Vec::new();
            for agent in &cfg.agents.agents {
                let mut per: Vec<(String, String)> = Vec::new();
                for channel in [
                    nexo_auth::handle::WHATSAPP,
                    nexo_auth::handle::TELEGRAM,
                    nexo_auth::handle::GOOGLE,
                ] {
                    if let Ok(handle) = bundle.resolver.resolve(&agent.id, channel) {
                        per.push((
                            channel.to_string(),
                            handle.account_id_raw().to_string(),
                        ));
                    }
                }
                bindings.push((agent.id.clone(), per));
            }
            Ok(Summary {
                accounts_wa: bundle.stores.whatsapp.list().len(),
                accounts_tg: bundle.stores.telegram.list().len(),
                accounts_google: bundle.stores.google.list().len(),
                warnings: bundle.warnings,
                errors: Vec::new(),
                bindings,
            })
        }
        Err(errs) => Ok(Summary {
            accounts_wa: 0,
            accounts_tg: 0,
            accounts_google: 0,
            warnings: Vec::new(),
            errors: errs.into_iter().map(|e| e.to_string()).collect(),
            bindings: Vec::new(),
        }),
    }
}

pub fn print(summary: &Summary) {
    println!();
    println!("── Credenciales (Phase 17) ─────────────────────");
    println!(
        "  cuentas: whatsapp={}  telegram={}  google={}",
        summary.accounts_wa, summary.accounts_tg, summary.accounts_google
    );
    if !summary.bindings.is_empty() {
        println!();
        println!("  bindings por agente:");
        for (agent, per) in &summary.bindings {
            if per.is_empty() {
                println!("    • {agent}: (sin credenciales — usará topics legacy)");
            } else {
                let rendered: Vec<String> =
                    per.iter().map(|(c, a)| format!("{c}={a}")).collect();
                println!("    • {agent}: {}", rendered.join(", "));
            }
        }
    }
    if !summary.warnings.is_empty() {
        println!();
        println!("  warnings:");
        for w in &summary.warnings {
            println!("    ⚠  {w}");
        }
    }
    if !summary.errors.is_empty() {
        println!();
        println!("  errores:");
        for e in &summary.errors {
            println!("    ✗  {e}");
        }
        println!();
        println!(
            "  Corrige los items anteriores antes de arrancar el daemon."
        );
        println!("  Re-corre con:  agent --config ./config --check-config");
    } else if summary.warnings.is_empty() {
        println!();
        println!("  ✔ credenciales OK.");
    }
}
