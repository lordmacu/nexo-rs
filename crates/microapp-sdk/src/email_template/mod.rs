//! Email-template builder primitives — block-based composer
//! similar to a stripped-down Elementor for email. Operator
//! authors templates from a fixed set of email-safe blocks
//! (heading, paragraph, button, image, divider, spacer,
//! two-column, list); the renderer emits 600-px-wide
//! table-based HTML with inline CSS that survives the lowest
//! common denominator email client (Outlook's Word renderer +
//! Gmail's mobile webview).
//!
//! Compose flow picks a template, renders with the recipient's
//! variables substituted, and uses the result as the outbound
//! body — same OutboundPublisher path as a manual compose.

pub mod asset_store;
pub mod blocks;
pub mod sanitize;
pub mod store;

pub use asset_store::{
    migrate as migrate_assets, mime_to_ext, AssetMetadata, AssetStore,
    AssetStoreError, StoredAsset, ALLOWED_MIMES, MAX_ASSET_BYTES,
};
pub use blocks::{
    migrate_legacy_to_rows, render_block, render_template,
    render_template_with_bg, render_template_with_page_bg,
    Column, EmailBlock, TextAlign,
};
// Image optimisation lifted to `nexo_media` — re-export under
// the same names so existing callers (admin/template_assets.rs)
// don't need a path change.
pub use nexo_media::{optimize as optimize_image, Optimized, OptimizeError};
pub use sanitize::email_safe_html;
pub use store::{
    migrate, EmailTemplate, EmailTemplateStore, TemplateStoreError,
};
