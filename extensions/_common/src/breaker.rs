use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug)]
pub struct Breaker {
    threshold: u32,
    open_for: Duration,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    state: State,
    fails: u32,
    open_until: Option<Instant>,
}

#[derive(Debug)]
pub enum BreakerError<E> {
    Rejected,
    Upstream(E),
}

impl Breaker {
    pub fn new(threshold: u32, open_for: Duration) -> Self {
        Self {
            threshold,
            open_for,
            inner: Mutex::new(Inner {
                state: State::Closed,
                fails: 0,
                open_until: None,
            }),
        }
    }

    pub fn call<F, R, E>(&self, f: F) -> Result<R, BreakerError<E>>
    where
        F: FnOnce() -> Result<R, E>,
    {
        // Acquire-and-update phase: decide if we admit the call.
        {
            let mut inner = self.inner.lock().expect("breaker poisoned");
            match inner.state {
                State::Closed | State::HalfOpen => {}
                State::Open => match inner.open_until {
                    Some(until) if Instant::now() >= until => {
                        inner.state = State::HalfOpen;
                        inner.open_until = None;
                    }
                    _ => return Err(BreakerError::Rejected),
                },
            }
        }

        match f() {
            Ok(v) => {
                let mut inner = self.inner.lock().expect("breaker poisoned");
                inner.state = State::Closed;
                inner.fails = 0;
                inner.open_until = None;
                Ok(v)
            }
            Err(e) => {
                let mut inner = self.inner.lock().expect("breaker poisoned");
                inner.fails = inner.fails.saturating_add(1);
                if inner.state == State::HalfOpen || inner.fails >= self.threshold {
                    inner.state = State::Open;
                    inner.open_until = Some(Instant::now() + self.open_for);
                }
                Err(BreakerError::Upstream(e))
            }
        }
    }

    pub fn reset(&self) {
        let mut inner = self.inner.lock().expect("breaker poisoned");
        inner.state = State::Closed;
        inner.fails = 0;
        inner.open_until = None;
    }

    #[cfg(test)]
    fn state(&self) -> State {
        self.inner.lock().unwrap().state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn closed_passes_through() {
        let b = Breaker::new(3, Duration::from_millis(50));
        let r: Result<u32, BreakerError<()>> = b.call(|| Ok(42));
        assert!(matches!(r, Ok(42)));
        assert_eq!(b.state(), State::Closed);
    }

    #[test]
    fn opens_after_threshold() {
        let b = Breaker::new(2, Duration::from_millis(50));
        let _: Result<(), _> = b.call(|| Err::<(), &str>("boom"));
        let _: Result<(), _> = b.call(|| Err::<(), &str>("boom"));
        assert_eq!(b.state(), State::Open);
        let r: Result<(), BreakerError<&str>> = b.call(|| Ok(()));
        assert!(matches!(r, Err(BreakerError::Rejected)));
    }

    #[test]
    fn half_open_after_cooldown_then_close() {
        let b = Breaker::new(1, Duration::from_millis(20));
        let _: Result<(), _> = b.call(|| Err::<(), &str>("boom"));
        assert_eq!(b.state(), State::Open);
        sleep(Duration::from_millis(25));
        let r: Result<u32, BreakerError<&str>> = b.call(|| Ok(7));
        assert!(matches!(r, Ok(7)));
        assert_eq!(b.state(), State::Closed);
    }

    #[test]
    fn half_open_failure_reopens() {
        let b = Breaker::new(1, Duration::from_millis(20));
        let _: Result<(), _> = b.call(|| Err::<(), &str>("boom"));
        sleep(Duration::from_millis(25));
        let r: Result<(), BreakerError<&str>> = b.call(|| Err("again"));
        assert!(matches!(r, Err(BreakerError::Upstream("again"))));
        assert_eq!(b.state(), State::Open);
    }
}
