//! Phase 83.5 — Conversational compliance primitives.
//!
//! Provider-agnostic, runtime-agnostic helpers every conversational
//! microapp needs. Each primitive ships as a small struct with one
//! evaluation method and zero IO. Microapps wire them into the
//! Phase 83.3 hook interceptor (vote-to-block / vote-to-transform)
//! to enforce policy without re-deriving the heuristics.
//!
//! No async runtime, no persistence, no LLM dep — pure logic.
//! Persistence (e.g. consent records, rate-limit token buckets) is
//! the microapp's responsibility; this crate gives you the
//! decision functions.
//!
//! Primitives:
//!
//! | Module | What it does |
//! |---|---|
//! | [`anti_loop`] | Detect same message N times in a window + auto-reply signatures |
//! | [`anti_manipulation`] | Match prompt-injection / role-hijack patterns |
//! | [`opt_out`] | Language-aware opt-out keyword detection |
//! | [`pii_redactor`] | Strip credit cards / phone numbers / emails before LLM sees text |
//! | [`rate_limit`] | Token bucket per user-key |
//! | [`consent_tracker`] | In-memory opt-in store + audit for GDPR / WhatsApp Business compliance |

pub mod anti_loop;
pub mod anti_manipulation;
pub mod consent_tracker;
pub mod opt_out;
pub mod pii_redactor;
pub mod rate_limit;

pub use anti_loop::{AntiLoopDetector, LoopVerdict};
pub use anti_manipulation::{AntiManipulationMatcher, ManipulationVerdict};
pub use consent_tracker::{ConsentRecord, ConsentStatus, ConsentTracker};
pub use opt_out::{OptOutMatcher, OptOutVerdict};
pub use pii_redactor::{PiiRedactor, RedactionStats};
pub use rate_limit::{RateLimitPerUser, RateLimitVerdict};
