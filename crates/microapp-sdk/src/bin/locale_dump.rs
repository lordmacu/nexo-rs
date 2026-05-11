//! Phase 81.19.b locale follow-up item 2 — dump every BCP-47
//! locale that `nexo_microapp_sdk::locale::Locale::from_str`
//! accepts as a JSON array. Consumed by
//! `scripts/lint-locale-list-sync.sh` to verify the operator-facing
//! `agent-creator-microapp/frontend/src/data/locales.ts`
//! `SUPPORTED_LOCALES` list is a SUBSET of the Rust accept-list
//! (curated frontend ≤ permissive Rust parser).
//!
//! Output shape (sorted, deterministic):
//! ```json
//! { "supported": ["de", "de-AR", "de-AU", ..., "zh-US"] }
//! ```

use nexo_microapp_sdk::locale::Locale;

fn main() {
    let mut supported: Vec<String> = Locale::iter_supported()
        .map(|l| l.as_bcp47().to_string())
        .collect();
    supported.sort();
    let payload = serde_json::json!({ "supported": supported });
    println!(
        "{}",
        serde_json::to_string(&payload).expect("JSON serialise")
    );
}
