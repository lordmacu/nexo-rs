use std::time::{Duration, Instant};

use nexo_driver_types::CancellationToken;
use tokio::time::sleep;

#[derive(Clone, Debug)]
pub struct ScheduledWake {
    pub duration_ms: u64,
    pub reason: String,
    pub sleep_started_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeResult {
    Fired { elapsed_ms: u64 },
    Cancelled,
}

pub fn build_tick_prompt(wake: &ScheduledWake, elapsed_ms: u64) -> String {
    format!(
        "<tick>\nkind: sleep_wake\nelapsed_ms: {elapsed_ms}\nreason: {}\n</tick>",
        wake.reason
    )
}

pub async fn wait_for_wake(wake: &ScheduledWake, cancel: &CancellationToken) -> WakeResult {
    let duration = Duration::from_millis(wake.duration_ms);
    tokio::select! {
        _ = sleep(duration) => WakeResult::Fired {
            elapsed_ms: wake.sleep_started_at.elapsed().as_millis() as u64,
        },
        _ = cancel.cancelled() => WakeResult::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_wake_tick_prompt_is_stable() {
        let wake = ScheduledWake {
            duration_ms: 270_000,
            reason: "waiting for new work".into(),
            sleep_started_at: std::time::Instant::now(),
        };
        let prompt = build_tick_prompt(&wake, 270_000);
        assert_eq!(
            prompt,
            "<tick>\nkind: sleep_wake\nelapsed_ms: 270000\nreason: waiting for new work\n</tick>"
        );
    }

    #[tokio::test]
    async fn wait_for_wake_stops_on_cancellation() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let wake = ScheduledWake {
            duration_ms: 60_000,
            reason: "idle".into(),
            sleep_started_at: std::time::Instant::now(),
        };
        let result = wait_for_wake(&wake, &cancel).await;
        assert!(matches!(result, WakeResult::Cancelled));
    }
}
