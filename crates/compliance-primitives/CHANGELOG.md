# Changelog — `nexo-compliance-primitives`

All notable changes to this crate are documented here per
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Semantic Versioning kicks in once `v1.0.0` ships; until then
breaking changes may land between 0.x releases.

## [0.1.0] — 2026-05-02

### Added

- Initial release. Six primitives:
  - `AntiLoopDetector { threshold, window }` — repetition
    counter + auto-reply signature matcher (Spanish + English
    defaults; custom signature lists supported).
  - `AntiManipulationMatcher` — case-insensitive substring
    match against curated phrase list covering instruction-
    override, role hijack, system-prompt extraction, jailbreak
    markers (Spanish + English).
  - `OptOutMatcher` — whole-word regex matching with word
    boundaries to avoid substring false positives.
  - `PiiRedactor` — strips phone numbers / credit cards /
    emails. Configurable Luhn check on cards. Per-category
    skip toggles. Returns `(redacted, RedactionStats)` for
    audit logging.
  - `RateLimitPerUser` — per-user token bucket with
    `Allowed { tokens_remaining }` / `Denied { retry_after }`
    verdicts.
  - `ConsentTracker` — in-memory opt-in/opt-out store with
    per-user audit trail. Default `Unknown` status means no
    outbound (CAN-SPAM / GDPR default-deny).
- 47 unit tests across the six primitives.
- Provider-agnostic + runtime-agnostic: zero deps on a specific
  LLM, no async runtime, no persistence.
