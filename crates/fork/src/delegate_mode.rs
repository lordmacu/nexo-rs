//! Delegation mode — Sync vs ForkAndForget. Step 80.19 / 2.
//!
//! Mirrors KAIROS's two distinct call patterns:
//! - `Sync` matches `runForkedAgent` awaited inline (used by sync
//!   delegation tools).
//! - `ForkAndForget` matches `runForkedAgent` spawned without await
//!   (used by autoDream Phase 80.1, AWAY_SUMMARY 80.14, eval harness).

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
