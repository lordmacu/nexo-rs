//! Phase 26 — pairing protocol.
//!
//! Two coexisting protocols:
//!
//! - **DM challenge** — opt-in inbound allowlist. A plugin (whatsapp,
//!   telegram, …) calls [`PairingGate::should_admit`] before publishing
//!   to the broker. Unknown senders get a one-time human-friendly code
//!   and the operator approves them via CLI / admin-ui.
//! - **Setup-code** — operator-initiated. `agent pair start` issues a
//!   short-lived HMAC-signed bearer token + a gateway URL, packs them
//!   into a base64url payload, and renders a QR. A companion app
//!   scans, opens the WS, presents the token, and gets a session
//!   token in return.
//!
//! This crate is a *leaf*: it does not depend on `nexo-core` or any
//! plugin crate. The bin (`src/main.rs`) wires the store + the gate
//! into the plugins, and registers the CLI subcommand.

pub mod adapter;
pub mod code;
pub mod gate;
pub mod qr;
pub mod registry;
pub mod setup_code;
pub mod store;
pub mod telemetry;
pub mod types;
pub mod url_resolver;

pub use adapter::PairingChannelAdapter;
pub use gate::PairingGate;
pub use registry::PairingAdapterRegistry;
pub use setup_code::SetupCodeIssuer;
pub use store::PairingStore;
pub use types::{
    ApprovedRequest, Decision, PairingError, PairingPolicy, PendingRequest, SetupCode, TokenClaims,
    UpsertOutcome,
};
