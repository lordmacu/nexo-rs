//! Phase 81.33.b.real Stage 6 — `[plugin.dashboard]` manifest
//! section.
//!
//! Plugins that want to surface in the setup wizard's channel
//! dashboard declare HOW the daemon detects their instances +
//! auth state via this section. Replaces the previous pattern
//! where each canonical channel (telegram / whatsapp / email)
//! had a hardcoded `Box<dyn ChannelDashboardSource>` impl in
//! `nexo-setup`.
//!
//! A `ManifestDashboardSource` in `nexo-setup` consumes this
//! section and runs the right discovery / auth check generically.
//! New canonical channels (signal, sms, …) add the manifest
//! section + their wizard surface appears automatically — no
//! `crates/setup/src/services/channels_dashboard.rs` impl
//! required.

use serde::{Deserialize, Serialize};

/// Daemon-side dashboard surface declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginDashboardSection {
    /// Instance enumeration strategy.
    pub layout: InstanceLayout,
    /// Auth-state probe strategy.
    pub auth_check: AuthCheck,
}

impl PluginDashboardSection {
    pub fn validate(&self) -> Result<(), String> {
        self.layout.validate()?;
        self.auth_check.validate()?;
        Ok(())
    }
}

/// How to enumerate instances for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstanceLayout {
    /// Single fixed instance labelled `"default"`. Used by
    /// telegram (one bot per agent) + email (one account per
    /// agent).
    Single,
    /// Walk `<workspace>/<agent>/<subdir>/<instance>/` for every
    /// directory entry. Used by whatsapp (multi-instance per
    /// agent in their workspace).
    WorkspaceWalk {
        /// Subdirectory under `<workspace>/<agent>/` containing
        /// instance directories. Example: `"whatsapp"` so the
        /// walker looks at `<workspace>/<agent>/whatsapp/<instance>/`.
        subdir: String,
    },
}

impl Default for InstanceLayout {
    fn default() -> Self {
        Self::Single
    }
}

impl InstanceLayout {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Single => Ok(()),
            Self::WorkspaceWalk { subdir } => {
                if subdir.is_empty() {
                    Err("WorkspaceWalk requires non-empty subdir".into())
                } else if subdir.contains('/') {
                    Err(format!(
                        "WorkspaceWalk subdir must be a single segment; got `{subdir}`"
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// How to probe auth state for an instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthCheck {
    /// Authenticated if `<secrets_dir>/<path>` exists + is
    /// non-empty. Used by telegram (`telegram_bot_token.txt`) +
    /// email (`email_password.txt`).
    FilePresence {
        /// Relative path under `secrets_dir`. Example:
        /// `"telegram_bot_token.txt"`.
        path: String,
    },
    /// Authenticated if the per-instance session directory
    /// contains ANY of `candidates`. Used by whatsapp
    /// (`session.db` / `state.db` / `device.json` /
    /// `registration.json`).
    SessionDirFiles {
        /// Candidate filenames inside the per-instance dir
        /// (joined under `<workspace>/<agent>/<subdir>/<instance>/`).
        candidates: Vec<String>,
    },
}

impl Default for AuthCheck {
    fn default() -> Self {
        Self::FilePresence {
            path: String::new(),
        }
    }
}

impl AuthCheck {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::FilePresence { path } => {
                if path.is_empty() {
                    Err("FilePresence requires non-empty path".into())
                } else if path.starts_with('/') {
                    Err(format!(
                        "FilePresence path must be relative to secrets_dir; got `{path}`"
                    ))
                } else {
                    Ok(())
                }
            }
            Self::SessionDirFiles { candidates } => {
                if candidates.is_empty() {
                    Err("SessionDirFiles requires at least one candidate filename".into())
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_telegram_shape() {
        let s = PluginDashboardSection {
            layout: InstanceLayout::Single,
            auth_check: AuthCheck::FilePresence {
                path: "telegram_bot_token.txt".into(),
            },
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_accepts_whatsapp_shape() {
        let s = PluginDashboardSection {
            layout: InstanceLayout::WorkspaceWalk {
                subdir: "whatsapp".into(),
            },
            auth_check: AuthCheck::SessionDirFiles {
                candidates: vec![
                    "session.db".into(),
                    "state.db".into(),
                    "device.json".into(),
                    "registration.json".into(),
                ],
            },
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_subdir() {
        let s = PluginDashboardSection {
            layout: InstanceLayout::WorkspaceWalk { subdir: "".into() },
            auth_check: AuthCheck::FilePresence {
                path: "foo.txt".into(),
            },
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_subdir_with_slash() {
        let s = PluginDashboardSection {
            layout: InstanceLayout::WorkspaceWalk {
                subdir: "foo/bar".into(),
            },
            auth_check: AuthCheck::FilePresence {
                path: "foo.txt".into(),
            },
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_absolute_file_path() {
        let s = PluginDashboardSection {
            layout: InstanceLayout::Single,
            auth_check: AuthCheck::FilePresence {
                path: "/etc/passwd".into(),
            },
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_candidates() {
        let s = PluginDashboardSection {
            layout: InstanceLayout::Single,
            auth_check: AuthCheck::SessionDirFiles {
                candidates: vec![],
            },
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn deserializes_telegram_toml() {
        let toml = r#"
            [layout]
            kind = "single"

            [auth_check]
            kind = "file_presence"
            path = "telegram_bot_token.txt"
        "#;
        let s: PluginDashboardSection = toml::from_str(toml).expect("parse");
        assert!(matches!(s.layout, InstanceLayout::Single));
        match s.auth_check {
            AuthCheck::FilePresence { path } => {
                assert_eq!(path, "telegram_bot_token.txt");
            }
            _ => panic!("expected FilePresence"),
        }
    }

    #[test]
    fn deserializes_whatsapp_toml() {
        let toml = r#"
            [layout]
            kind = "workspace_walk"
            subdir = "whatsapp"

            [auth_check]
            kind = "session_dir_files"
            candidates = ["session.db", "state.db"]
        "#;
        let s: PluginDashboardSection = toml::from_str(toml).expect("parse");
        match s.layout {
            InstanceLayout::WorkspaceWalk { subdir } => {
                assert_eq!(subdir, "whatsapp");
            }
            _ => panic!("expected WorkspaceWalk"),
        }
    }
}
