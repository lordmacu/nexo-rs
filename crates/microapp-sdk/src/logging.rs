//! Tracing setup helper — replaces ~10 LOC every microapp
//! repeats verbatim.

/// Initialise `tracing-subscriber` from an env var.
///
/// `crate_name` is uppercased (with `-` → `_`) and suffixed with
/// `_LOG`; the resulting key is read as the env-filter directive.
/// Defaults to `info` when the env var is absent.
///
/// Output goes to stderr with `with_target(false)` for compact
/// formatting. Idempotent — calling twice is a no-op (the second
/// `try_init` returns `Err` which is ignored).
///
/// # Example
///
/// ```no_run
/// nexo_microapp_sdk::init_logging_from_env("agent-creator");
/// // Reads `AGENT_CREATOR_LOG`, defaults to `info`.
/// ```
pub fn init_logging_from_env(crate_name: &str) {
    let env_key = env_key_for(crate_name);
    let filter = tracing_subscriber::EnvFilter::try_from_env(&env_key)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

fn env_key_for(crate_name: &str) -> String {
    let mut out = String::with_capacity(crate_name.len() + 4);
    for c in crate_name.chars() {
        out.push(if c == '-' { '_' } else { c.to_ascii_uppercase() });
    }
    out.push_str("_LOG");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_uppercases_and_replaces_dashes() {
        assert_eq!(env_key_for("agent-creator"), "AGENT_CREATOR_LOG");
        assert_eq!(env_key_for("ventas_etb"), "VENTAS_ETB_LOG");
        assert_eq!(env_key_for("simple"), "SIMPLE_LOG");
    }

    #[test]
    fn init_is_idempotent() {
        // Second call must not panic. `try_init` returns Err on
        // second call; we ignore. Test passes if no panic.
        init_logging_from_env("test-crate");
        init_logging_from_env("test-crate");
    }
}
