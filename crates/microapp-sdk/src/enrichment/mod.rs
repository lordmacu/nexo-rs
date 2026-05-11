// Enrichment primitives for microapps that build per-sender
// profiles (CRM-shaped, vendor onboarding, partner portals).
// Lifted from `nexo-rs-extension-marketing` so any microapp
// resolving "who is this email/domain" reuses the contract.
//
// Three pillars in one feature:
//   - domain_classifier: 200+ public providers + 30 disposable
//     hosts, tenant-agnostic curated list. classify(domain)
//     returns DomainKind.
//   - cache: TTL-keyed (tenant_id, domain) sqlite cache so
//     enrichments don't re-fetch on every inbound. Per-tenant
//     by design — empresa A's enrichment never bleeds to B.
//   - fallback: trait + chain runner so the consumer composes
//     N adapters (signature parser, LLM extractor, cross-thread
//     linker, paid API) in priority order, stopping at the
//     first source meeting a confidence threshold.

#![allow(missing_docs)]

//! Enrichment primitives — domain classifier, TTL cache,
//! fallback chain runner.

pub mod cache;
pub mod domain_classifier;
pub mod fallback;

pub use cache::{EnrichmentCache, SqliteEnrichmentCache};
pub use domain_classifier::{classify, DomainKind};
pub use fallback::{
    EnrichmentInput, EnrichmentSource, EnrichmentSourceError, FallbackChain, FallbackOutcome,
    SourceCost,
};

pub use nexo_tool_meta::marketing::EnrichmentResult;
