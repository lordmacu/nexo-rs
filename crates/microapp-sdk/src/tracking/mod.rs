//! Engagement tracking primitives (M15.23.a.1).
//!
//! Reusable building blocks for any microapp / extension that
//! needs **open + click tracking** on outbound channel content
//! (email today, push notifications + SMS later). Three layers:
//!
//! - [`token`] — HMAC-signed URL tokens. Operator stamps a
//!   per-tenant secret at boot; the signer mints a tag for the
//!   `(tenant_id, msg_id, link_id?)` triple, the ingest route
//!   verifies in constant time. Forged URLs are 401, replayed
//!   URLs hit the (caller-owned) idempotency layer.
//! - [`html`] — Pure-rust HTML helpers: `inject_pixel(html,
//!   pixel_url)` appends the 1×1 GIF tag before `</body>`;
//!   `rewrite_links(html, ...)` swaps every `<a href="X">` for
//!   the click-redirector URL and yields the `(link_id ↔ X)`
//!   mapping the caller persists.
//! - [`types`] — Newtype wrappers (`MsgId`, `LinkId`) +
//!   event records (`OpenEvent`, `ClickEvent`) + the 1×1
//!   transparent GIF blob the ingest route serves.
//!
//! ## Threat model
//!
//! Tags are **HMAC-SHA256 truncated to 16 bytes**, base64url
//! encoded — 2¹²⁸ forgery search space. Constant-time compare
//! via [`subtle`] guards against timing oracles. The signer's
//! input domain is `b"\x01" || tenant_id || b"\x00" || msg_id
//! || b"\x00" || link_id?`; the leading byte is a version
//! prefix so future format changes can be migrated cleanly.
//!
//! Cross-tenant replay is impossible — `tenant_id` is part of
//! the signed payload, so tenant A cannot bind a tag from
//! tenant B's secret. Cross-microapp replay is impossible —
//! every microapp picks its own secret at boot.
//!
//! ## Crate-level zero-cost when unused
//!
//! Everything in this module is gated behind the `tracking`
//! feature. Microapps that don't need engagement tracking pay
//! zero dep cost (no `hmac`, no `sha2`, no `regex`).

mod html;
mod token;
mod types;

pub use html::{inject_pixel, rewrite_links, RewriteOutcome};
pub use token::{TokenError, TrackingToken, TrackingTokenSigner};
pub use types::{
    ClickEvent, LinkId, LinkMapping, MsgId, OpenEvent, PIXEL_GIF_BYTES,
    PIXEL_GIF_CONTENT_TYPE,
};
