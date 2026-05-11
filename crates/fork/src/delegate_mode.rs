//! Delegation mode — Sync vs ForkAndForget.
//!
//! Two distinct call patterns:
//! - `Sync` matches a forked agent awaited inline (used by sync
//!   delegation tools).
//! - `ForkAndForget` matches a forked agent spawned without await
//!   (used by autoDream, AWAY_SUMMARY digests, the eval harness).

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DelegateMode {
    /// Block until completion. Returns ForkResult.
    Sync,
    /// Spawn + return ForkHandle immediately. Caller awaits when ready,
    /// or ignores entirely (true fire-and-forget).
    ForkAndForget,
}

impl DelegateMode {
    pub fn is_fire_and_forget(self) -> bool {
        matches!(self, Self::ForkAndForget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_and_forget_predicate() {
        assert!(DelegateMode::ForkAndForget.is_fire_and_forget());
        assert!(!DelegateMode::Sync.is_fire_and_forget());
    }

    #[test]
    fn serde_roundtrip() {
        let v = serde_json::to_string(&DelegateMode::Sync).unwrap();
        assert_eq!(v, r#""Sync""#);
        let back: DelegateMode = serde_json::from_str(&v).unwrap();
        assert_eq!(back, DelegateMode::Sync);
    }
}
