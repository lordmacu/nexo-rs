//! Per-agent credential management for channel plugins (WhatsApp,
//! Telegram, Google). Exposes [`CredentialHandle`], per-channel stores,
//! and an [`AgentCredentialResolver`] that binds an agent id to the
//! account it is allowed to use for outbound traffic.
//!
//! Boot-time [`gauntlet`] validates filesystem paths, permissions, and
//! cross-store consistency, accumulating every error in one pass so
//! operators can fix the full YAML in a single edit.

pub mod audit;
pub mod breaker;
pub mod email;
pub mod error;
pub mod gauntlet;
pub mod google;
pub mod handle;
pub mod resolver;
pub mod store;
pub mod telegram;
pub mod telemetry;
pub mod whatsapp;
pub mod wire;

pub use breaker::{BreakerRegistry, BreakerState};
pub use wire::{build_credentials, load_google_auth, print_report, CredentialsBundle};

pub use error::{BuildError, CredentialError, ResolveError};
pub use handle::{AgentId, Channel, CredentialHandle, Fingerprint};
pub use resolver::{AgentCredentialResolver, AgentCredentialsInput, CredentialStores, StrictLevel};
pub use store::{CredentialStore, ValidationReport};

pub use email::{
    load_email_secrets, EmailAccount, EmailAuth, EmailCredentialStore,
};
