//! First-run wizard scaffolding for microapps with operator UIs.
//!
//! Two pieces, both gated behind the `wizard` feature:
//!
//! - [`probe`] — server-side LLM key validation
//!   ([`ProbeRequest`] / [`ProbeResult`] / [`probe::probe`]).
//!   The wizard's API-key step posts the plaintext key here; the
//!   microapp probes `GET {base_url}/models` and ships back only
//!   `{ ok, status, latency_ms, model_count, error? }` so the raw
//!   key never leaves the backend.
//! - [`cache::CachedSnapshot`] — generic TTL snapshot cache. The
//!   bootstrap endpoint uses this to fan out parallel admin RPCs
//!   once per 5 s window across N concurrent browser tabs;
//!   microapps with their own bootstrap shape parameterise on
//!   `T: Clone + Send + Sync`.
//!
//! HTTP routes (the actual `POST /api/onboarding/llm/probe` and
//! `GET /api/bootstrap` handlers) stay in the microapp because
//! the bootstrap response shape is domain-specific (agent-creator
//! returns `{ has_llm, has_pairing, has_agent, providers,
//! paired_devices }`; another microapp would surface different
//! flags).

pub mod cache;
pub mod probe;

pub use cache::CachedSnapshot;
pub use probe::{probe, ProbeRequest, ProbeResult, DEFAULT_PROBE_TIMEOUT};
