// Identity primitives for microapps that resolve "who sent this
// message" against a persistent store. Lifted from the
// `nexo-rs-extension-marketing` extension as part of M15.3 so
// any future microapp (lead gen, support routing, partner
// portal) reuses the contract instead of re-implementing it.
//
// Tenant-keyed from day 1 — every method takes a `tenant_id`
// and the default sqlite impl uses composite indexes
// `(tenant_id, email)` so empresa A's persons can never appear
// in empresa B's lookups.

#![allow(missing_docs)]

//! Identity store traits + sqlite default impl.
//!
//! Re-exports the wire-shape types from `nexo-tool-meta::marketing`
//! (`Person`, `Company`, `PersonId`, `CompanyId`) so consumers
//! don't pull duplicate types.

pub mod error;
pub mod jid;
pub mod store;

pub use jid::{normalize_jid, parse_jid, same_user, JidParseError, ParsedJid};

pub use error::IdentityError;
pub use store::{
    open_pool, CompanyStore, LidPnMapping, LidPnMappingStore, PersonEmail, PersonEmailStore,
    PersonPhone, PersonPhoneStore, PersonStore, SqliteCompanyStore, SqliteLidPnMappingStore,
    SqlitePersonEmailStore, SqlitePersonPhoneStore, SqlitePersonStore,
};

// Re-export the wire types so consumers don't need to depend
// on `nexo-tool-meta` directly when they only need identity.
pub use nexo_tool_meta::marketing::{Company, CompanyId, Person, PersonId};
