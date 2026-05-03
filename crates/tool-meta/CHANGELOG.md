# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-tool-meta-v0.1.0) - 2026-05-03

### Added

- *(82.10.m)* SDK transparent operator_token_hash stamping
- *(82.10.l)* admin/llm_providers/probe — daemon-side LLM provider reachability check
- *(82.10.k)* nexo/admin/secrets/write — atomic secret persist + std::env::set_var injection
- *(82.13.b.firehose)* emit ProcessingStateChanged on pause/resume
- *(82.14.b.firehose)* EscalationRequested + EscalationResolved variants
- *(83.8.12.6)* skills per-tenant layout — <root>/{__global__,<tenant_id>}/<name>/SKILL.md
- *(83.8.12.5.c)* admin RPC llm_providers wire shapes for tenant scope (handler stubs reject)
- *(82.13.b.3.1)* ProcessingControlStore push_pending/drain_pending + cap policy + drop event
- *(82.13.b.2)* processing/resume injects operator summary as System transcript entry + SDK release end-to-end
- *(82.13.b.1.1)* TranscriptAppender trait + ProcessingInterventionParams.session_id
- *(83.8.12.4)* tenant_id filter wire shapes + agents handler enforcement
- *(83.8.12.2)* tenants domain handler + capability + INVENTORY
- *(83.8.12.1)* empresa wire shapes + BindingContext + AgentConfig empresa_id
- *(83.8.11)* docs + admin-ui sync + close-out
- *(83.8.5)* EscalationReason::UnknownQuery variant
- *(83.8.1)* admin/skills CRUD wire shapes in nexo-tool-meta
- *(83.16)* MicroappError notification wire shape + 6 tests
- *(82.14.1)* escalation state + admin RPC params
- *(82.13.1)* processing pause + intervention wire shapes
- *(82.12.1)* http_server capability + TokenRotated wire shapes
- *(82.11.1)* agent_events wire shapes in nexo-tool-meta
- *(82.10.f)* llm_providers + channels domains
- *(82.10.e)* pairing domain (start/status/cancel + notify wire)
- *(82.10.d)* credentials domain (list/register/revoke many-to-many)
- *(82.10.c)* agents domain (list/get/upsert/delete)
- *(82.7.e)* wire-up llm_behavior + Phase 72 rate_limited marker
- *(82.5.a)* InboundMessageMeta shape + signature change
- *(82.3.1)* format_dispatch_source helper for Phase 72 marker
- *(82.4.3)* BindingContext gains event_source field
- *(82.4.2)* nexo-tool-meta — EventSourceMeta + Phase 72 marker
- *(82.4.1)* nexo-tool-meta — mustache-lite template renderer
- *(82.2.b.7)* nexo-webhook-receiver re-exports envelope from tool-meta
- *(82.2.b.5)* nexo-tool-meta — lib root + README + missing-docs gate
- *(82.2.b.4)* nexo-tool-meta — WebhookEnvelope + format helper
- *(82.2.b.3)* nexo-tool-meta — _meta builder + parser
- *(82.2.b.2)* nexo-tool-meta — BindingContext type + helpers
- *(82.2.b.1)* nexo-tool-meta crate skeleton

### Other

- *(tool-meta)* export PAIRING_STATUS_NOTIFY_METHOD const for pairing notification listeners
- *(83.8.12.1.fix)* rename empresa → tenant for English code identifiers
