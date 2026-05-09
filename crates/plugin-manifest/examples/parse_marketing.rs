fn main() {
    let v = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    for path in &[
        "/home/familia/chat/agent-creator-microapp/.dev-state/plugins/marketing/plugin.toml",
        "/home/familia/chat/agent-creator-microapp/.dev-state/plugins/browser-0.2.1/nexo-plugin.toml",
    ] {
        println!("--- {} ---", path);
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => { eprintln!("READ: {e}"); continue; }
        };
        let m: nexo_plugin_manifest::PluginManifest = match toml::from_str(&raw) {
            Ok(m) => m,
            Err(e) => { eprintln!("PARSE: {e}"); continue; }
        };
        let mut errs = Vec::new();
        nexo_plugin_manifest::validate::run_all(&m, &v, &mut errs);
        if errs.is_empty() {
            println!("OK id={} version={}", m.plugin.id, m.plugin.version);
        } else {
            for e in errs { eprintln!("VALIDATE: {e}"); }
        }
    }
}
