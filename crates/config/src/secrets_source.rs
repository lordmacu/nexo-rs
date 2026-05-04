//! Phase 82.10.s — read-only `SecretsSource` trait.
//!
//! `nexo-config` consumes secrets at config-load to resolve
//! per-provider API keys from `api_key_secret_id` references. Kept
//! sync + `Send + Sync` so config types can call it from any
//! context (boot path, reload signal, integration tests).
//!
//! `nexo-setup`'s `FsSecretsStore` impls both this AND the async
//! write-side `SecretsStore` trait in `nexo-core`.

use std::io;

/// Read-only secrets accessor used by `LlmProviderConfig::resolve_api_key`
/// and equivalents. Returns the secret's plaintext value when found.
pub trait SecretsSource: Send + Sync {
    /// Read the secret keyed by `id`. Returns `Ok(value)` when the
    /// store has a non-empty entry; `Err(NotFound)` when absent;
    /// other `io::Error` variants on permission / IO failure.
    fn read(&self, id: &str) -> io::Result<String>;
}

/// `Box<dyn SecretsSource>` forwards directly via blanket impl so
/// callers can pass either an owned trait object or a borrowed one.
impl<T: SecretsSource + ?Sized> SecretsSource for Box<T> {
    fn read(&self, id: &str) -> io::Result<String> {
        (**self).read(id)
    }
}

impl<T: SecretsSource + ?Sized> SecretsSource for std::sync::Arc<T> {
    fn read(&self, id: &str) -> io::Result<String> {
        (**self).read(id)
    }
}

/// In-memory test double — holds a static `(id → value)` map.
/// Lives here (not behind `cfg(test)`) so downstream crates can
/// reuse it for their own test suites.
#[derive(Default, Debug, Clone)]
pub struct InMemorySecretsSource {
    entries: std::collections::HashMap<String, String>,
}

impl InMemorySecretsSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, id: impl Into<String>, value: impl Into<String>) -> Self {
        self.entries.insert(id.into(), value.into());
        self
    }

    pub fn insert(&mut self, id: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(id.into(), value.into());
    }
}

impl SecretsSource for InMemorySecretsSource {
    fn read(&self, id: &str) -> io::Result<String> {
        self.entries
            .get(id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("secret {id} not found")))
    }
}

/// Empty source — every read returns `NotFound`. Used as the
/// default when callers don't have a real store wired yet (e.g.
/// legacy boot paths that only support env-expanded yaml keys).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSecretsSource;

impl SecretsSource for NoSecretsSource {
    fn read(&self, id: &str) -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("secrets source not configured (id requested: {id})"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_round_trips_known_id() {
        let s = InMemorySecretsSource::new().with("foo", "bar");
        assert_eq!(s.read("foo").unwrap(), "bar");
    }

    #[test]
    fn in_memory_returns_not_found_for_unknown_id() {
        let s = InMemorySecretsSource::new();
        let err = s.read("missing").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn no_secrets_source_always_errors() {
        let err = NoSecretsSource.read("anything").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn arc_box_blanket_impls_forward_correctly() {
        let arc: std::sync::Arc<dyn SecretsSource> =
            std::sync::Arc::new(InMemorySecretsSource::new().with("k", "v"));
        assert_eq!(arc.read("k").unwrap(), "v");
        let boxed: Box<dyn SecretsSource> =
            Box::new(InMemorySecretsSource::new().with("k", "v"));
        assert_eq!(boxed.read("k").unwrap(), "v");
    }
}
