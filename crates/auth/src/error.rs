use std::path::PathBuf;

use thiserror::Error;

use crate::handle::{Channel, Fingerprint};

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("account '{account}' not found in {channel} store")]
    NotFound {
        channel: Channel,
        account: String,
    },

    #[error("agent '{agent}' not permitted on {channel}:{fp}")]
    NotPermitted {
        channel: Channel,
        agent: String,
        fp: Fingerprint,
    },

    #[error(
        "credential file '{path}' has insecure permissions (mode {mode:o}); run `chmod 600 {path}`",
        path = path.display()
    )]
    InsecurePermissions { path: PathBuf, mode: u32 },

    #[error("credential file missing: {path}", path = path.display())]
    FileMissing { path: PathBuf },

    #[error("credential file unreadable ({path}): {source}", path = path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("google token expired and no refresh_token; run setup wizard for account '{account}'")]
    GoogleExpired { account: String },
}

/// Errors collected by the boot-time gauntlet. The resolver builder
/// accumulates these in a `Vec` so every misconfiguration surfaces in
/// one pass rather than one-per-run.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error(
        "duplicate credential path: '{path}' used by both {a_channel}:{a_instance} and {b_channel}:{b_instance}",
        path = path.display()
    )]
    DuplicatePath {
        path: PathBuf,
        a_channel: Channel,
        a_instance: String,
        b_channel: Channel,
        b_instance: String,
    },

    #[error(
        "overlapping session_dir: '{inner}' is a sub-path of '{outer}' — both would collide on Signal keys",
        inner = inner.display(), outer = outer.display()
    )]
    PathPrefixOverlap { outer: PathBuf, inner: PathBuf },

    #[error("agent '{agent}' binds credentials.{channel}='{account}' but no such {channel} instance exists (available: {available:?})")]
    MissingInstance {
        channel: Channel,
        agent: String,
        account: String,
        available: Vec<String>,
    },

    #[error("agent '{agent}' listens on multiple {channel} instances {instances:?} but did not declare credentials.{channel}; declare it explicitly")]
    AmbiguousOutbound {
        channel: Channel,
        agent: String,
        instances: Vec<String>,
    },

    #[error("{channel} instance '{instance}' allow_agents excludes '{agent}' but that agent declares credentials.{channel}='{instance}'")]
    AllowAgentsExcludes {
        channel: Channel,
        instance: String,
        agent: String,
    },

    #[error("agent '{agent}': credentials.{channel}='{outbound}' but inbound binding is '{inbound}' — asymmetric; silence with credentials.{channel}_asymmetric: true")]
    AsymmetricBinding {
        channel: Channel,
        agent: String,
        outbound: String,
        inbound: String,
    },

    #[error("{channel} instance '{instance}': {source}")]
    Credential {
        channel: Channel,
        instance: String,
        #[source]
        source: CredentialError,
    },
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("agent '{agent}' has no credential bound for channel '{channel}'")]
    Unbound { agent: String, channel: Channel },

    #[error(transparent)]
    Credential(#[from] CredentialError),
}
