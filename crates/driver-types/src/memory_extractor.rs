//! Phase M4 — provider-agnostic memory-extraction hook.
//!
//! Mirrors the [`AutoDreamHook`] (Phase 80.1.b) and
//! [`MemoryCheckpointer`] (Phase 80.1.g) cycle-break patterns:
//! the trait is declared upstream so consumer crates can hold
//! `Arc<dyn MemoryExtractor>` without depending on the producer
//! crate that ships the concrete implementation.
//!
//! Today `nexo-driver-loop` ships
//! `impl MemoryExtractor for ExtractMemories` (Phase 77.5
//! production extractor). Both turn engines hold an
//! `Arc<dyn MemoryExtractor>` so post-turn extraction fires
//! uniformly across paths:
//! - `nexo-driver-loop::orchestrator` — Phase 67 self-driving
//!   agents (was the only path before M4).
//! - `nexo-core::agent::LlmAgentBehavior` — every regular
//!   agent (event-driven inbound, pollers, heartbeat,
//!   marketing plugin, etc.).
//!
//! Provider-agnostic: the trait operates on transcript text +
//! a destination directory. The concrete extractor uses an
//! `Arc<dyn LlmClient>` upstream so any provider impl works
//! (Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI /
//! Mistral) per the `feedback_provider_agnostic.md` rule.
//!
//! IRROMPIBLE refs:
//! - claude-code-leak
//!   `services/extractMemories/extractMemories.ts:121-148`
//!   `hasMemoryWritesSince` — gate cadence semantics the
//!   inherent `check_gates` mirrors. Already cited on the
//!   producer side (`crates/driver-loop/src/extract_memories.rs`).
//! - claude-code-leak `QueryEngine.ts` — leak's single turn
//!   engine fires extract after every turn. Our split
//!   driver-loop / LlmAgentBehavior shares `MemoryExtractor`
//!   so both paths trigger uniformly without duplicating the
//!   gate / coalesce logic.
//! - `research/` — no relevant prior art (OpenClaw is
//!   channel-side, no extract-memories concept).
//!
//! [`AutoDreamHook`]: crate::auto_dream::AutoDreamHook
//! [`MemoryCheckpointer`]: crate::memory_checkpoint::MemoryCheckpointer

use std::path::PathBuf;
use std::sync::Arc;

use crate::GoalId;

/// Hook fired post-turn to extract durable memories from the
/// conversation transcript. Implementations carry their own
/// gate logic (throttle, in-progress mutex, circuit breaker)
/// so callers can fire on every turn without thinking about
/// cadence.
///
/// Cycle-break: this trait lives in `nexo-driver-types` (no
/// concrete impl) so `nexo-core` and `nexo-driver-loop` can
/// both depend on the trait without one depending on the
/// other.
pub trait MemoryExtractor: Send + Sync + 'static {
    /// Increment the per-turn counter. Called every turn
    /// regardless of whether extraction actually fires — the
    /// throttle window is internal.
    fn tick(&self);

    /// Spawn extraction with internal gate checks. Errors are
    /// logged and absorbed; this method NEVER blocks the
    /// caller (extraction runs on a background task).
    ///
    /// `messages_text` is the recent transcript serialized for
    /// the prompt. `memory_dir` is the destination root
    /// (e.g. `~/.nexo/<agent>/memory/`). `goal_id` is the
    /// stable handle for the conversation (driver-loop passes
    /// the actual goal; regular agents pass
    /// `GoalId(session_id)`). `turn_index` is informational
    /// for telemetry — `0` is a valid sentinel when the caller
    /// does not track per-session turn counters.
    fn extract(
        self: Arc<Self>,
        goal_id: GoalId,
        turn_index: u32,
        messages_text: String,
        memory_dir: PathBuf,
    );
}
