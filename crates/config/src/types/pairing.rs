//! Optional `config/pairing.yaml` schema. FOLLOWUPS PR-6.
//!
//! Until this lands the daemon hardcodes:
//!   - store path:      `<memory_dir>/pairing.db`
//!   - secret path:     `~/.nexo/secret/pairing.key`
//!   - default TTL:     60 seconds
//!   - public_url:      `--public-url` CLI flag only
//!   - cleartext allow: built-in list (loopback / RFC1918 /
//!                      link-local / `.local` / `10.0.2.2`)
//!
//! The file is **optional**. When absent the daemon keeps the
//! legacy defaults; when present each field overrides selectively
//! so an operator only writes what they actually want to change.
//! Containerised deploys typically only need `storage.path` to point
//! at a mounted volume.

use serde::Deserialize;

/// Top-level wrapper. The file is `pairing:` rooted so a single
/// `config/pairing.yaml` doesn't accidentally collide with other
/// section names if an operator merges configs.
///
/// ```yaml
/// # config/pairing.yaml
/// pairing:
///   storage:
///     path: /var/lib/nexo/pairing/pairing.db
///   setup_code:
///     secret_path: /var/lib/nexo/pairing/pairing.key
///     default_ttl_secs: 600
///   public_url: wss://nexo.example.com/pair
///   ws_cleartext_allow:
///     - kitchen-pi.local
///     - my.cool.host
/// ```
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PairingConfig {
    pub pairing: PairingInner,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PairingInner {
    /// Where the SQLite pairing database lives. `None` keeps the
    /// legacy default of `<memory_dir>/pairing.db`.
    #[serde(default)]
    pub storage: PairingStorageConfig,

    /// Setup-code issuer config. `None` keeps the legacy default
    /// (`~/.nexo/secret/pairing.key`, 60s TTL).
    #[serde(default)]
    pub setup_code: PairingSetupCodeConfig,

    /// Operator-pinned public URL. Highest-priority override in the
    /// `nexo pair start` URL resolver chain. `None` falls through
    /// to `tunnel.url` → `gateway.remote.url` → LAN bind.
    #[serde(default)]
    pub public_url: Option<String>,

    /// Extra hostnames where cleartext `ws://` is allowed (on top
    /// of the built-in loopback / RFC1918 / link-local / `.local`
    /// list). Empty by default.
    #[serde(default)]
    pub ws_cleartext_allow: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PairingStorageConfig {
    /// Override the SQLite pairing database path. `None` keeps
    /// `<memory_dir>/pairing.db`. Containerised deploys typically
    /// set this to a mounted volume path like
    /// `/var/lib/nexo/pairing/pairing.db`.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PairingSetupCodeConfig {
    /// Override the HMAC secret key file path. `None` keeps
    /// `~/.nexo/secret/pairing.key`. The file is auto-generated on
    /// first boot with mode 0600.
    pub secret_path: Option<String>,

    /// Override the setup-code TTL (default 60s). Setup codes are
    /// short-lived bearer tokens; bumping past a few minutes is
    /// usually wrong.
    #[serde(default)]
    pub default_ttl_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_yaml_yields_defaults() {
        let parsed: PairingConfig = serde_yaml::from_str("pairing: {}").unwrap();
        assert!(parsed.pairing.storage.path.is_none());
        assert!(parsed.pairing.setup_code.secret_path.is_none());
        assert!(parsed.pairing.setup_code.default_ttl_secs.is_none());
        assert!(parsed.pairing.public_url.is_none());
        assert!(parsed.pairing.ws_cleartext_allow.is_empty());
    }

    #[test]
    fn full_yaml_round_trips() {
        let yaml = r#"
pairing:
  storage:
    path: /var/lib/nexo/pairing/pairing.db
  setup_code:
    secret_path: /var/lib/nexo/pairing/pairing.key
    default_ttl_secs: 600
  public_url: wss://nexo.example.com/pair
  ws_cleartext_allow:
    - kitchen-pi.local
    - my.cool.host
"#;
        let parsed: PairingConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            parsed.pairing.storage.path.as_deref(),
            Some("/var/lib/nexo/pairing/pairing.db"),
        );
        assert_eq!(
            parsed.pairing.setup_code.secret_path.as_deref(),
            Some("/var/lib/nexo/pairing/pairing.key"),
        );
        assert_eq!(
            parsed.pairing.setup_code.default_ttl_secs,
            Some(600),
        );
        assert_eq!(
            parsed.pairing.public_url.as_deref(),
            Some("wss://nexo.example.com/pair"),
        );
        assert_eq!(parsed.pairing.ws_cleartext_allow.len(), 2);
        assert!(parsed
            .pairing
            .ws_cleartext_allow
            .contains(&"kitchen-pi.local".to_string()));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = "pairing:\n  unknown_field: 1\n";
        let result: Result<PairingConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_unknown_storage_field() {
        let yaml = "pairing:\n  storage:\n    bad: 1\n";
        let result: Result<PairingConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }
}
