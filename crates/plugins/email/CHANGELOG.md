# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/lordmacu/nexo-rs/compare/nexo-plugin-email-v0.1.1...nexo-plugin-email-v0.1.2) - 2026-05-03

### Fixed

- *(email)* collapse dsn field match guards for clippy

### Other

- stabilize workspace: complete mcp/followup wiring and satisfy strict CI lints
- CI fix: silence clippy on in-flux Phase 76 + 79 + 48 scaffolding
- Phase 48 email follow-up: use-case sweep across plugin + tools + setup
- Phase 48 use cases: thread_root + thread_session_id per email_search row
- Phase 48 use cases: email_search query.from_domain + query.has_attachments
- Phase 48 use cases: email_attachment_get + email_health + email_instances_list
- Phase 48 use cases: email_get + email_thread + email_bounces_summary + strict_bounce_check
- Phase 48 audit #3 #3: bounce orphan + retention prune
- Phase 48 audit #3 #4: trim_dlq cross-process invariant doc
- Phase 48 setup wizard UX polish: menu loop + connectivity probe
- Phase 48 audit #3 fixes: move_to_dlq tombstone order + empty recipient guard
- Phase 48 audit #2 fixes: empty-From reply + hot-reload cred validation
- Phase 48 audit fixes: DSN sender-domain harden + boot connectivity probe
- Phase 48 audit fixes: DLQ cap + attachment / parse error metrics
- Phase 48 audit fixes: SQLite synchronous + GC race
- Phase 48 follow-up #3 (partial): in-process pipeline tests
- Phase 48 follow-up #5: surgical hot-reload (close add-only gap)
- Phase 48 follow-up #5 (partial): account-diff + add-only hot-reload
- Phase 48 follow-up #9: binding-policy auto-filter
- Phase 48 follow-up #10: attachment ref-count + retention GC
- Phase 48 follow-up #8: dedicated Prometheus metrics
- Phase 48 follow-up #4: persistent bounce history
- Phase 48 follow-up #2: IMAP STARTTLS
- Phase 48 follow-up #6: multi-selector DKIM probe
- Phase 48 follow-up #1: register email_* tools in main.rs
- Phase 48.10: close Phase 48 — main.rs wiring + capabilities + docs
- Phase 48.9.a: SPF/DKIM boot check + provider hint
- Phase 48.8: loop-prevention + DSN/bounce parsing
- Phase 48.7.c: archive/move_to/label/search + IMAP wrappers (closes 48.7)
- Phase 48.7.b: email_send + email_reply handlers
- Phase 48.7.a: tool context + DispatcherHandle + IMAP op helpers
- Phase 48.6: thread_root_id + session-id v5 + reply enrichment
- Phase 48.5.c: outbound multipart builder + enqueue read
- Phase 48.5.b: inbound MIME parser + drain enrichment
- Phase 48.5.a: MIME envelope foundations (events, deps, config)
- Phase 48.4.b: SMTP dispatcher + lettre wiring
- Phase 48.4.a: outbound foundations (events, mime_text, queue)
- Phase 48.3.b: IMAP IDLE worker + reconnect/backoff/cursor
- Phase 48.3.a: email plugin foundations (events, health, cursor)
- Phase 48.1: email plugin scaffold + multi-account config
- *(crates)* expand 6 READMEs with project-context block + richer detail
- *(release)* per-crate independent versioning
