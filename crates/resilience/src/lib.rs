//! Shared fault-tolerance primitives.
//!
//! [`CircuitBreaker`] guards calls to flaky external dependencies (LLM APIs,
//! browser CDP, message broker). Three states — `Closed`, `Open`, `HalfOpen` —
//! with exponential backoff and automatic probing.
//!
//! # Example
//!
//! ```
//! use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};
//!
//! let cb = CircuitBreaker::new("github-api", CircuitBreakerConfig::default());
//! assert!(cb.allow());
//! cb.on_success();
//! ```

#![deny(missing_docs)]

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tunables for [`CircuitBreaker`]. Defaults match production
/// posture for typical external API guards (5 failures → open,
/// 2 successes → close, 10s initial backoff capped at 2 min).
///
/// Intentionally **not** `#[non_exhaustive]`: this struct is
/// caller-populated and external callers must be able to build
/// it via struct literal. Field additions are semver-major.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures while `Closed` before tripping `Open`.
    pub failure_threshold: u32,
    /// Consecutive successes while `HalfOpen` before closing.
    pub success_threshold: u32,
    /// First `Open` window after a clean → tripped transition.
    pub initial_backoff: Duration,
    /// Cap on the doubled backoff after repeated `HalfOpen → Open`
    /// failures. Stops runaway exponential growth.
    pub max_backoff: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            initial_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(120),
        }
    }
}

#[derive(Debug)]
enum State {
    Closed {
        consecutive_failures: u32,
    },
    Open {
        until: Instant,
        backoff: Duration,
    },
    HalfOpen {
        consecutive_successes: u32,
        backoff: Duration,
    },
}

/// Outcome of a [`CircuitBreaker::call`] invocation. Either the
/// breaker rejected the call without invoking the closure
/// ([`CircuitError::Open`]) or the closure ran and returned its
/// own typed error ([`CircuitError::Inner`]).
///
/// Intentionally **not** `#[non_exhaustive]`: this is a utility
/// error wrapper that downstream callers pattern-match
/// exhaustively. Adding a third variant is a deliberate semver
/// signal that the breaker now has a new short-circuit reason
/// callers must handle.
#[derive(Debug, thiserror::Error)]
pub enum CircuitError<E> {
    /// The breaker is currently `Open`; the closure was not run.
    /// The argument is the breaker's `name` for log correlation.
    #[error("circuit breaker `{0}` is open")]
    Open(String),

    /// The closure executed and returned its own error.
    #[error(transparent)]
    Inner(E),
}

/// State machine guarding one external dependency.
///
/// Cheap to clone via `Arc`; methods are `&self` so a single
/// instance can be shared across tasks. Internally synchronised
/// via `Mutex`, with poison recovery — a panic elsewhere in the
/// process can never leave the breaker permanently locked out.
pub struct CircuitBreaker {
    name: String,
    config: CircuitBreakerConfig,
    state: Mutex<State>,
}

/// Acquire the mutex, recovering from poisoning rather than cascading
/// the panic. A resilience primitive must not itself become a single
/// point of failure — if a panic somewhere else (tracing, allocator)
/// poisoned the lock during a state transition, we'd rather keep the
/// breaker running on the (still-valid) stored state than crash every
/// subsequent call. The state enum itself is a simple value type with
/// no partial-write corruption risk, so recovering is safe.
fn lock_state(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("circuit breaker mutex was poisoned — recovering inner state");
        poisoned.into_inner()
    })
}

impl CircuitBreaker {
    /// Build a fresh breaker in `Closed` state.
    pub fn new(name: impl Into<String>, config: CircuitBreakerConfig) -> Self {
        Self {
            name: name.into(),
            config,
            state: Mutex::new(State::Closed {
                consecutive_failures: 0,
            }),
        }
    }

    /// Operator-visible identifier used in tracing logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns true if the breaker currently rejects calls.
    pub fn is_open(&self) -> bool {
        let state = lock_state(&self.state);
        matches!(&*state, State::Open { until, .. } if Instant::now() < *until)
    }

    /// Check whether a call may proceed. If the breaker is `Open` and the
    /// backoff has elapsed, transitions to `HalfOpen` and returns `true` so
    /// the caller may probe.
    pub fn allow(&self) -> bool {
        let mut state = lock_state(&self.state);
        match &*state {
            State::Closed { .. } | State::HalfOpen { .. } => true,
            State::Open { until, backoff } => {
                if Instant::now() >= *until {
                    let backoff = *backoff;
                    *state = State::HalfOpen {
                        consecutive_successes: 0,
                        backoff,
                    };
                    tracing::info!(name = %self.name, "circuit breaker: open → half-open");
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful call. While `HalfOpen`, transitions to
    /// `Closed` after `success_threshold` consecutive successes.
    pub fn on_success(&self) {
        let mut state = lock_state(&self.state);
        match &mut *state {
            State::Closed {
                consecutive_failures,
            } => {
                *consecutive_failures = 0;
            }
            State::HalfOpen {
                consecutive_successes,
                ..
            } => {
                *consecutive_successes += 1;
                if *consecutive_successes >= self.config.success_threshold {
                    tracing::info!(name = %self.name, "circuit breaker: half-open → closed");
                    *state = State::Closed {
                        consecutive_failures: 0,
                    };
                }
            }
            State::Open { .. } => {}
        }
    }

    /// Record a failed call. While `Closed`, increments the
    /// failure counter and trips `Open` once the threshold is
    /// reached. While `HalfOpen`, immediately reopens with a
    /// doubled backoff. Calls while already `Open` extend the
    /// backoff (defense against external floods after trip).
    pub fn on_failure(&self) {
        let mut state = lock_state(&self.state);
        match &*state {
            State::Closed {
                consecutive_failures,
            } => {
                let next = consecutive_failures + 1;
                if next >= self.config.failure_threshold {
                    let backoff = self.config.initial_backoff;
                    tracing::warn!(name = %self.name, failures = next, "circuit breaker: closed → open");
                    *state = State::Open {
                        until: Instant::now() + backoff,
                        backoff,
                    };
                } else {
                    *state = State::Closed {
                        consecutive_failures: next,
                    };
                }
            }
            State::HalfOpen { backoff, .. } => {
                let new_backoff = (*backoff * 2).min(self.config.max_backoff);
                tracing::warn!(name = %self.name, backoff_secs = new_backoff.as_secs(), "circuit breaker: half-open → open (probe failed)");
                *state = State::Open {
                    until: Instant::now() + new_backoff,
                    backoff: new_backoff,
                };
            }
            State::Open { backoff, .. } => {
                let new_backoff = (*backoff * 2).min(self.config.max_backoff);
                *state = State::Open {
                    until: Instant::now() + new_backoff,
                    backoff: new_backoff,
                };
            }
        }
    }

    /// Force the breaker into `Open` with the initial backoff. Used when
    /// an out-of-band signal (e.g. broker disconnect) indicates the
    /// dependency is down regardless of per-call failures.
    pub fn trip(&self) {
        let mut state = lock_state(&self.state);
        let backoff = match &*state {
            State::Open { backoff, .. } | State::HalfOpen { backoff, .. } => {
                (*backoff * 2).min(self.config.max_backoff)
            }
            State::Closed { .. } => self.config.initial_backoff,
        };
        tracing::warn!(name = %self.name, backoff_secs = backoff.as_secs(), "circuit breaker tripped");
        *state = State::Open {
            until: Instant::now() + backoff,
            backoff,
        };
    }

    /// Force the breaker back to `Closed` with zero failures. Used when an
    /// out-of-band signal confirms the dependency is healthy.
    pub fn reset(&self) {
        let mut state = lock_state(&self.state);
        tracing::info!(name = %self.name, "circuit breaker reset");
        *state = State::Closed {
            consecutive_failures: 0,
        };
    }

    /// Convenience wrapper: run `f`, record success/failure automatically,
    /// and short-circuit with `CircuitError::Open` when the breaker rejects.
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, CircuitError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        if !self.allow() {
            return Err(CircuitError::Open(self.name.clone()));
        }
        match f().await {
            Ok(v) => {
                self.on_success();
                Ok(v)
            }
            Err(e) => {
                self.on_failure();
                Err(CircuitError::Inner(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fast_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(200),
        }
    }

    #[test]
    fn opens_after_threshold() {
        let cb = CircuitBreaker::new("t", fast_config());
        assert!(cb.allow());
        cb.on_failure();
        assert!(cb.allow());
        cb.on_failure();
        assert!(!cb.allow());
        assert!(cb.is_open());
    }

    #[test]
    fn success_resets_failure_count() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.on_failure();
        cb.on_success();
        cb.on_failure(); // only 1 consecutive failure again
        assert!(cb.allow());
    }

    #[tokio::test]
    async fn transitions_to_half_open_after_backoff() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.on_failure();
        cb.on_failure();
        assert!(cb.is_open());

        tokio::time::sleep(Duration::from_millis(30)).await;
        // allow() lazily transitions Open → HalfOpen when the backoff elapses
        assert!(cb.allow());
        assert!(!cb.is_open());
    }

    #[tokio::test]
    async fn half_open_success_closes() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.on_failure();
        cb.on_failure();
        tokio::time::sleep(Duration::from_millis(30)).await;

        assert!(cb.allow()); // → HalfOpen
        cb.on_success();
        cb.on_success();
        // Now Closed — failures need to cross threshold again
        cb.on_failure();
        assert!(cb.allow());
    }

    #[tokio::test]
    async fn half_open_failure_reopens_with_bigger_backoff() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.on_failure();
        cb.on_failure();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(cb.allow()); // → HalfOpen
        cb.on_failure(); // → Open, backoff doubled to ~40ms

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!cb.allow()); // still open (backoff is longer now)
    }

    #[tokio::test]
    async fn call_short_circuits_when_open() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.trip();
        let result: Result<(), CircuitError<&str>> = cb.call(|| async { Ok::<(), &str>(()) }).await;
        assert!(matches!(result, Err(CircuitError::Open(_))));
    }

    #[tokio::test]
    async fn call_records_success_and_failure() {
        let cb = CircuitBreaker::new("t", fast_config());
        let _: Result<(), CircuitError<&str>> = cb.call(|| async { Err("boom") }).await;
        let _: Result<(), CircuitError<&str>> = cb.call(|| async { Err("boom") }).await;
        assert!(cb.is_open());
    }

    #[test]
    fn trip_and_reset_are_idempotent() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.trip();
        assert!(cb.is_open());
        cb.reset();
        assert!(!cb.is_open());
        cb.reset();
        assert!(!cb.is_open());
    }

    #[tokio::test]
    async fn trip_while_open_doubles_backoff() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.trip(); // first trip, backoff = 20ms
        tokio::time::sleep(Duration::from_millis(15)).await;
        cb.trip(); // second trip, backoff = 40ms (doubled, capped at 200ms)
                   // Still well inside 40ms window
        assert!(cb.is_open());
        tokio::time::sleep(Duration::from_millis(15)).await;
        // 30ms into the 40ms window — still open (would have been
        // HalfOpen already with the original 20ms backoff)
        assert!(cb.is_open());
    }

    #[test]
    fn on_failure_while_open_extends_backoff() {
        let cb = CircuitBreaker::new("t", fast_config());
        cb.trip();
        assert!(cb.is_open());
        // Signalling another failure should NOT crash and should keep
        // us open with a longer backoff (defensive-guard against
        // external signals flooding in while we're already tripped).
        cb.on_failure();
        cb.on_failure();
        assert!(cb.is_open());
    }

    #[test]
    fn recovers_from_poisoned_mutex() {
        // Simulate a poisoned lock by panicking inside a lock-holding
        // closure. The standard `Mutex::lock()` would return Err after
        // this; our `lock_state` helper unwraps the poison and keeps
        // going so a panic in tracing / allocation elsewhere can't
        // cascade-cripple the breaker.
        use std::sync::Arc;
        let cb = Arc::new(CircuitBreaker::new("t", fast_config()));
        let cb2 = cb.clone();
        let h = std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = cb2.state.lock().unwrap();
                panic!("simulated panic while holding lock");
            }));
        });
        h.join().unwrap();
        // Mutex is now poisoned. But our helpers should still work.
        assert!(!cb.is_open()); // Closed by default
        cb.on_failure();
        cb.on_failure();
        assert!(
            cb.is_open(),
            "breaker should have tripped despite poisoned mutex"
        );
    }
}
