// Routing rules engine for microapps that dispatch inbound
// events to handlers per declarative rules. Lifted from the
// `nexo-rs-extension-marketing` extension as part of M15.4 so
// the AST + dispatcher stay reusable across CRM-shaped
// extensions and any future microapp that wants
// declarative-rules routing.
//
// Tenant-scoped from day 1: rule sets carry a `TenantIdRef`,
// the dispatcher refuses to evaluate rules across tenants.

#![allow(missing_docs)]

//! Declarative routing rules: predicate AST + tenant-scoped
//! dispatcher.
//!
//! Re-exports the wire-shape types from `nexo-tool-meta::marketing`
//! (`RoutingRule`, `RuleSet`, `RulePredicate`, `AssignTarget`,
//! `DomainKind`) so consumers don't pull duplicate types.

pub mod dispatch;
pub mod yaml_loader;

pub use dispatch::{Dispatcher, MatchContext, RoutingDecision, RoutingError};
pub use yaml_loader::{load_rule_set_from_str, RuleSetYaml};

pub use nexo_tool_meta::marketing::{
    AssignTarget, DomainKind, RulePredicate, RuleSet, RoutingRule, TenantIdRef,
    VendedorId,
};
