//! Cooperative cancellation token, opaquely wrapped over
//! `tokio_util::sync::CancellationToken` so the backend can change
//! without touching the trait surface.

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    inner: tokio_util::sync::CancellationToken,
}

impl CancellationToken {
    /// Create a fresh, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Has the token been signalled?
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Signal cancellation. Idempotent.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Future that resolves once cancellation is signalled. Useful in
    /// `tokio::select!` against subprocess streams.
    pub async fn cancelled(&self) {
        self.inner.cancelled().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_not_cancelled() {
        let t = CancellationToken::new();
        assert!(!t.is_cancelled());
    }

    #[test]
    fn cancel_flips_state() {
        let t = CancellationToken::new();
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn clone_shares_state() {
        let a = CancellationToken::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled());
    }
}
