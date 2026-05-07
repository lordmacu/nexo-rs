use thiserror::Error;

/// Typed error surface for the identity store. Discriminated
/// union so callers `match` on `kind` instead of parsing
/// strings.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum IdentityError {
    /// Underlying SQLite operation failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),

    /// Caller passed a `tenant_id` that doesn't match the
    /// store's tenant scope. Defense-in-depth: stores already
    /// filter by `tenant_id` in queries; this catches a bug
    /// where a caller accidentally hands a foreign tenant.
    #[error("tenant_unauthorised: caller passed {got:?}, store scoped to {expected:?}")]
    TenantUnauthorised {
        expected: String,
        got: String,
    },

    /// Migration failed at first open. Corrupt schema or sqlx
    /// version drift.
    #[error("migration: {0}")]
    Migration(String),

    /// `email` validation failed (empty, doesn't contain `@`,
    /// over 320 chars per RFC 5321). Stores reject these so
    /// downstream lookups never normalise differently.
    #[error("invalid email: {0:?}")]
    InvalidEmail(String),
}
