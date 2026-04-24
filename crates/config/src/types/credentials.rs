//! Per-agent credential bindings + optional `google-auth.yaml`.
//!
//! Agents declare `credentials.<channel>` to pin outbound traffic to
//! a specific plugin instance / Google account. Only the channel
//! names that actually appear are deserialised; adding a fourth
//! channel does not require bumping the schema here.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Map of `channel -> account_id`. Parsed from `credentials:` inside
/// `agents.d/*.yaml`. Keys are free-form strings so new channels (LINE,
/// Discord, …) do not need a new struct field.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentCredentialsConfig {
    #[serde(default)]
    pub whatsapp: Option<String>,
    #[serde(default)]
    pub telegram: Option<String>,
    #[serde(default)]
    pub google: Option<String>,

    /// Channel names for which the operator accepts an asymmetric
    /// inbound ≠ outbound binding. Silences the gauntlet warning.
    /// Expressed as a map for YAML ergonomics (`whatsapp_asymmetric: true`).
    #[serde(default, flatten)]
    pub asymmetric: HashMap<String, serde_yaml::Value>,
}

impl AgentCredentialsConfig {
    /// Extract boolean flags of the form `<channel>_asymmetric: true`.
    pub fn asymmetric_flags(&self) -> HashMap<String, bool> {
        let mut out = HashMap::new();
        for (k, v) in &self.asymmetric {
            if let Some(chan) = k.strip_suffix("_asymmetric") {
                if let Some(b) = v.as_bool() {
                    out.insert(chan.to_string(), b);
                }
            }
        }
        out
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleAuthFile {
    pub google_auth: GoogleAuthConfig,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct GoogleAuthConfig {
    #[serde(default)]
    pub accounts: Vec<GoogleAccountConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GoogleAccountConfig {
    /// Account id used by `agents[].credentials.google` to bind. Free
    /// form — conventionally the Google email address.
    pub id: String,
    /// 1:1 agent that owns this account. Gauntlet rejects cross-agent
    /// binding in V1.
    pub agent_id: String,
    pub client_id_path: PathBuf,
    pub client_secret_path: PathBuf,
    pub token_path: PathBuf,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialises_minimal_credentials_block() {
        let yaml = r#"
            whatsapp: personal
            telegram: ana_bot
        "#;
        let cfg: AgentCredentialsConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.whatsapp.as_deref(), Some("personal"));
        assert_eq!(cfg.telegram.as_deref(), Some("ana_bot"));
        assert_eq!(cfg.google, None);
    }

    #[test]
    fn parses_asymmetric_flags() {
        let yaml = r#"
            whatsapp: a
            whatsapp_asymmetric: true
        "#;
        let cfg: AgentCredentialsConfig = serde_yaml::from_str(yaml).unwrap();
        let flags = cfg.asymmetric_flags();
        assert_eq!(flags.get("whatsapp").copied(), Some(true));
    }

    #[test]
    fn empty_credentials_is_default() {
        let yaml = "{}";
        let cfg: AgentCredentialsConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.whatsapp.is_none());
        assert!(cfg.telegram.is_none());
        assert!(cfg.google.is_none());
    }

    #[test]
    fn google_auth_file_parses() {
        let yaml = r#"
            google_auth:
              accounts:
                - id: ana@x.com
                  agent_id: ana
                  client_id_path: /secrets/cid
                  client_secret_path: /secrets/csec
                  token_path: /secrets/tok
                  scopes:
                    - https://www.googleapis.com/auth/gmail.readonly
        "#;
        let file: GoogleAuthFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(file.google_auth.accounts.len(), 1);
        assert_eq!(file.google_auth.accounts[0].id, "ana@x.com");
        assert_eq!(file.google_auth.accounts[0].agent_id, "ana");
        assert_eq!(file.google_auth.accounts[0].scopes.len(), 1);
    }
}
