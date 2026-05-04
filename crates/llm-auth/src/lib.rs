//! Reusable OAuth + PKCE primitives for Nexo LLM providers.
//!
//! Pure async / no terminal I/O / no file persistence beyond what
//! `bundle::OAuthBundle::to_json_pretty` produces. Caller wraps
//! these with stdin glue (CLI wizard) or admin RPC handlers
//! (`oauth_start` / `oauth_finish`).
//!
//! See [`pkce`] for the building blocks every flow shares,
//! [`anthropic`] for the Claude.ai authorization-code flow,
//! [`minimax`] for the Token-Plan device-code flow,
//! [`bundle`] for the typed on-disk shape, and
//! [`verifier_store`] for the single-use cross-request session
//! state used by the admin RPC dispatcher.

#![warn(missing_docs)]

pub mod anthropic;
pub mod bundle;
pub mod minimax;
pub mod pkce;
pub mod verifier_store;

pub use bundle::{BundleValidationError, OAuthBundle};
pub use pkce::{gen_pkce, parse_code_payload, pct_encode, ParseCodeError, Pkce, StateEncoding};
pub use verifier_store::{
    DeviceCodeContext, InMemoryVerifierStore, SessionStatus, VerifierEntry, VerifierStore,
};
