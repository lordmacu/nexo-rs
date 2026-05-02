//! Phase 81.5 — re-export of the operator-facing config from
//! `nexo-config` plus the env-var expansion helper consumed by
//! [`super::discover`].

use std::path::{Path, PathBuf};

pub use nexo_config::PluginDiscoveryConfig;

use super::report::{
    DiagnosticLevel, DiscoveryDiagnostic, DiscoveryDiagnosticKind,
};

/// Expand `$NEXO_HOME` and `$HOME` references inside each search path.
/// Returns the expanded paths in order plus an `UnresolvedEnvVar`
/// diagnostic for every search-path that referenced a missing var.
///
/// Only those two vars are honored — supporting arbitrary `$VAR`
/// expansion would let the operator silently leak environment state
/// into the discovery contract. Two well-known vars cover the cases
/// surfaced by Phase 81 design.
pub fn resolve_search_paths(
    cfg: &PluginDiscoveryConfig,
) -> (Vec<PathBuf>, Vec<DiscoveryDiagnostic>) {
    let mut resolved = Vec::with_capacity(cfg.search_paths.len());
    let mut diagnostics = Vec::new();
    for raw in &cfg.search_paths {
        match expand(raw) {
            Ok(p) => resolved.push(p),
            Err(var_name) => {
                diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: raw.clone(),
                    kind: DiscoveryDiagnosticKind::UnresolvedEnvVar {
                        var_name,
                        in_path: raw.clone(),
                    },
                });
            }
        }
    }
    (resolved, diagnostics)
}

fn expand(raw: &Path) -> Result<PathBuf, String> {
    let s = raw.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    let mut rest = &*s;
    while let Some(idx) = rest.find('$') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 1..];
        let var_end = after
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(after.len());
        let var = &after[..var_end];
        if var.is_empty() {
            // bare `$` — leave it alone
            out.push('$');
            rest = after;
            continue;
        }
        match std::env::var(var) {
            Ok(v) => out.push_str(&v),
            Err(_) => return Err(var.to_string()),
        }
        rest = &after[var_end..];
    }
    out.push_str(rest);
    Ok(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_var() {
        // SAFETY: tests run sequentially within the file; we restore
        // after the assertion.
        unsafe { std::env::set_var("NEXO_TEST_RESOLVE_VAR", "/opt/nexo") };
        let cfg = PluginDiscoveryConfig {
            search_paths: vec![PathBuf::from("$NEXO_TEST_RESOLVE_VAR/plugins")],
            ..Default::default()
        };
        let (paths, diags) = resolve_search_paths(&cfg);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/opt/nexo/plugins"));
        assert!(diags.is_empty());
        unsafe { std::env::remove_var("NEXO_TEST_RESOLVE_VAR") };
    }

    #[test]
    fn unresolved_var_emits_diagnostic() {
        unsafe { std::env::remove_var("NEXO_TEST_UNRESOLVED_VAR") };
        let cfg = PluginDiscoveryConfig {
            search_paths: vec![PathBuf::from("$NEXO_TEST_UNRESOLVED_VAR/plugins")],
            ..Default::default()
        };
        let (paths, diags) = resolve_search_paths(&cfg);
        assert!(paths.is_empty());
        assert_eq!(diags.len(), 1);
        match &diags[0].kind {
            DiscoveryDiagnosticKind::UnresolvedEnvVar { var_name, .. } => {
                assert_eq!(var_name, "NEXO_TEST_UNRESOLVED_VAR");
            }
            other => panic!("expected UnresolvedEnvVar, got {other:?}"),
        }
    }

    #[test]
    fn mixed_resolved_and_unresolved() {
        unsafe { std::env::set_var("NEXO_TEST_MIX_RESOLVED", "/x") };
        unsafe { std::env::remove_var("NEXO_TEST_MIX_UNRESOLVED") };
        let cfg = PluginDiscoveryConfig {
            search_paths: vec![
                PathBuf::from("$NEXO_TEST_MIX_RESOLVED/a"),
                PathBuf::from("$NEXO_TEST_MIX_UNRESOLVED/b"),
                PathBuf::from("/literal/c"),
            ],
            ..Default::default()
        };
        let (paths, diags) = resolve_search_paths(&cfg);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/x/a"));
        assert_eq!(paths[1], PathBuf::from("/literal/c"));
        assert_eq!(diags.len(), 1);
        unsafe { std::env::remove_var("NEXO_TEST_MIX_RESOLVED") };
    }
}
