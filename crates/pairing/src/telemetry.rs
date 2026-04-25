//! Phase 26.y — Prometheus telemetry for the pairing flow.
//!
//! Counters and one push-tracked gauge that close out follow-up PR-2:
//!
//! - `pairing_approvals_total{channel,result}` — counter
//! - `pairing_codes_expired_total` — counter
//! - `pairing_bootstrap_tokens_issued_total{profile}` — counter
//! - `pairing_requests_pending{channel}` — gauge
//!
//! Layering note: this crate is a leaf (no `nexo-core` dep) so the
//! metric primitives ship here and `nexo_core::telemetry::render_prometheus`
//! stitches the [`render`] block into the exposition response. Same
//! pattern as `nexo_web_search::telemetry`.
//!
//! The gauge is push-tracked: `inc/dec` from each lifecycle event and
//! [`set`] for an authoritative refresh. Drift after a daemon crash is
//! recovered by calling [`crate::refresh_pending_gauge`] against the
//! store.

use dashmap::DashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::LazyLock;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ApprovalKey {
    channel: String,
    result: String,
}

static APPROVALS: LazyLock<DashMap<ApprovalKey, AtomicU64>> = LazyLock::new(DashMap::new);
static CODES_EXPIRED: AtomicU64 = AtomicU64::new(0);
static BOOTSTRAP_TOKENS_ISSUED: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
static REQUESTS_PENDING: LazyLock<DashMap<String, AtomicI64>> = LazyLock::new(DashMap::new);

/// Bump the pairing-approval counter. `result` is one of
/// `"ok" | "expired" | "not_found"`. `channel` is the empty string for
/// `not_found` (the row was never located so its channel is unknown).
pub fn inc_approvals(channel: &str, result: &str) {
    APPROVALS
        .entry(ApprovalKey {
            channel: channel.to_string(),
            result: result.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Add `n` expirations to the pruned-codes counter. Bumped from two
/// sites: per row deleted by [`crate::PairingStore::purge_expired`] and
/// once per `approve` call that finds a row past TTL.
pub fn add_codes_expired(n: u64) {
    CODES_EXPIRED.fetch_add(n, Ordering::Relaxed);
}

/// Bump the bootstrap-token issuance counter. `profile` reflects the
/// claims profile that the token will carry.
pub fn inc_bootstrap_tokens_issued(profile: &str) {
    BOOTSTRAP_TOKENS_ISSUED
        .entry(profile.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Increment the pending-requests gauge for `channel`.
pub fn inc_requests_pending(channel: &str) {
    REQUESTS_PENDING
        .entry(channel.to_string())
        .or_insert_with(|| AtomicI64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Decrement the pending-requests gauge for `channel`. Clamps at 0 —
/// going negative would mean the bookkeeping drifted (a daemon crash
/// between insert and `+1`, or a double `dec`). We stay at 0 and
/// rely on [`crate::refresh_pending_gauge`] for authoritative recovery
/// rather than trusting an underflow.
pub fn dec_requests_pending(channel: &str) {
    let entry = REQUESTS_PENDING
        .entry(channel.to_string())
        .or_insert_with(|| AtomicI64::new(0));
    let mut current = entry.load(Ordering::Relaxed);
    loop {
        if current <= 0 {
            entry.store(0, Ordering::Relaxed);
            return;
        }
        match entry.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// Subtract `n` from the pending-requests gauge for `channel` (clamped
/// at 0). Used by `purge_expired` to batch a per-channel decrement.
pub fn sub_requests_pending(channel: &str, n: i64) {
    if n <= 0 {
        return;
    }
    let entry = REQUESTS_PENDING
        .entry(channel.to_string())
        .or_insert_with(|| AtomicI64::new(0));
    let mut current = entry.load(Ordering::Relaxed);
    loop {
        let next = (current - n).max(0);
        match entry.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// Authoritative set of the pending-requests gauge for `channel`.
/// Called by [`crate::refresh_pending_gauge`] from a `SELECT COUNT(*)`
/// result so a process restart resyncs without relying on
/// inc/dec bookkeeping.
pub fn set_requests_pending(channel: &str, value: i64) {
    REQUESTS_PENDING
        .entry(channel.to_string())
        .or_insert_with(|| AtomicI64::new(0))
        .store(value.max(0), Ordering::Relaxed);
}

/// Snapshot of every channel currently tracked by the gauge. Used by
/// the refresh routine to zero out channels that no longer have any
/// pending rows.
pub fn pending_channels() -> Vec<String> {
    REQUESTS_PENDING.iter().map(|e| e.key().clone()).collect()
}

// === Test helpers — read counter values. ===

pub fn approvals_total(channel: &str, result: &str) -> u64 {
    APPROVALS
        .get(&ApprovalKey {
            channel: channel.to_string(),
            result: result.to_string(),
        })
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

pub fn codes_expired_total() -> u64 {
    CODES_EXPIRED.load(Ordering::Relaxed)
}

pub fn bootstrap_tokens_issued_total(profile: &str) -> u64 {
    BOOTSTRAP_TOKENS_ISSUED
        .get(profile)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

pub fn requests_pending(channel: &str) -> i64 {
    REQUESTS_PENDING
        .get(channel)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Reset every 26.y counter — test-only. The 26.x counter
/// (`pairing_inbound_challenged_total`) lives in `nexo-core` and is
/// not touched here.
pub fn reset_for_test() {
    APPROVALS.clear();
    CODES_EXPIRED.store(0, Ordering::Relaxed);
    BOOTSTRAP_TOKENS_ISSUED.clear();
    REQUESTS_PENDING.clear();
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Append the pairing 26.y metrics block to a Prometheus exposition
/// buffer. Called by `nexo_core::telemetry::render_prometheus`.
pub fn render(out: &mut String) {
    out.push_str(
        "# HELP pairing_approvals_total Pairing approval attempts by channel and result.\n",
    );
    out.push_str("# TYPE pairing_approvals_total counter\n");
    if APPROVALS.is_empty() {
        out.push_str("pairing_approvals_total{channel=\"\",result=\"\"} 0\n");
    } else {
        let mut rows: Vec<(ApprovalKey, u64)> = APPROVALS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.channel.clone(), a.0.result.clone())
                .cmp(&(b.0.channel.clone(), b.0.result.clone()))
        });
        for (k, v) in rows {
            out.push_str(&format!(
                "pairing_approvals_total{{channel=\"{}\",result=\"{}\"}} {}\n",
                escape(&k.channel),
                escape(&k.result),
                v
            ));
        }
    }

    out.push_str("# HELP pairing_codes_expired_total Pairing setup codes pruned past TTL or rejected as expired on approve.\n");
    out.push_str("# TYPE pairing_codes_expired_total counter\n");
    out.push_str(&format!(
        "pairing_codes_expired_total {}\n",
        CODES_EXPIRED.load(Ordering::Relaxed)
    ));

    out.push_str(
        "# HELP pairing_bootstrap_tokens_issued_total Bootstrap tokens minted by profile.\n",
    );
    out.push_str("# TYPE pairing_bootstrap_tokens_issued_total counter\n");
    if BOOTSTRAP_TOKENS_ISSUED.is_empty() {
        out.push_str("pairing_bootstrap_tokens_issued_total{profile=\"\"} 0\n");
    } else {
        let mut rows: Vec<(String, u64)> = BOOTSTRAP_TOKENS_ISSUED
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (profile, v) in rows {
            out.push_str(&format!(
                "pairing_bootstrap_tokens_issued_total{{profile=\"{}\"}} {}\n",
                escape(&profile),
                v
            ));
        }
    }

    out.push_str(
        "# HELP pairing_requests_pending Pending pairing requests by channel (push-tracked).\n",
    );
    out.push_str("# TYPE pairing_requests_pending gauge\n");
    if REQUESTS_PENDING.is_empty() {
        out.push_str("pairing_requests_pending{channel=\"\"} 0\n");
    } else {
        let mut rows: Vec<(String, i64)> = REQUESTS_PENDING
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (channel, v) in rows {
            out.push_str(&format!(
                "pairing_requests_pending{{channel=\"{}\"}} {}\n",
                escape(&channel),
                v
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn approvals_inc_and_read() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        inc_approvals("whatsapp", "ok");
        inc_approvals("whatsapp", "ok");
        inc_approvals("telegram", "expired");
        assert_eq!(approvals_total("whatsapp", "ok"), 2);
        assert_eq!(approvals_total("telegram", "expired"), 1);
        assert_eq!(approvals_total("whatsapp", "expired"), 0);
    }

    #[test]
    fn codes_expired_accumulates() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        add_codes_expired(3);
        add_codes_expired(2);
        assert_eq!(codes_expired_total(), 5);
    }

    #[test]
    fn bootstrap_tokens_per_profile() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        inc_bootstrap_tokens_issued("default");
        inc_bootstrap_tokens_issued("staging");
        inc_bootstrap_tokens_issued("default");
        assert_eq!(bootstrap_tokens_issued_total("default"), 2);
        assert_eq!(bootstrap_tokens_issued_total("staging"), 1);
    }

    #[test]
    fn requests_pending_inc_dec_clamp() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        inc_requests_pending("whatsapp");
        inc_requests_pending("whatsapp");
        assert_eq!(requests_pending("whatsapp"), 2);
        dec_requests_pending("whatsapp");
        assert_eq!(requests_pending("whatsapp"), 1);
        dec_requests_pending("whatsapp");
        dec_requests_pending("whatsapp"); // would underflow
        assert_eq!(requests_pending("whatsapp"), 0);
    }

    #[test]
    fn requests_pending_sub_clamp() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        for _ in 0..5 {
            inc_requests_pending("telegram");
        }
        sub_requests_pending("telegram", 3);
        assert_eq!(requests_pending("telegram"), 2);
        sub_requests_pending("telegram", 99); // clamps
        assert_eq!(requests_pending("telegram"), 0);
    }

    #[test]
    fn set_pending_authoritative() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        set_requests_pending("whatsapp", 7);
        assert_eq!(requests_pending("whatsapp"), 7);
        set_requests_pending("whatsapp", 0);
        assert_eq!(requests_pending("whatsapp"), 0);
        set_requests_pending("whatsapp", -3); // clamps
        assert_eq!(requests_pending("whatsapp"), 0);
    }

    #[test]
    fn render_zero_when_empty() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        let mut s = String::new();
        render(&mut s);
        assert!(s.contains("pairing_approvals_total{channel=\"\",result=\"\"} 0"));
        assert!(s.contains("pairing_codes_expired_total 0"));
        assert!(s.contains("pairing_bootstrap_tokens_issued_total{profile=\"\"} 0"));
        assert!(s.contains("pairing_requests_pending{channel=\"\"} 0"));
    }

    #[test]
    fn render_emits_values() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        inc_approvals("whatsapp", "ok");
        add_codes_expired(4);
        inc_bootstrap_tokens_issued("default");
        set_requests_pending("telegram", 2);
        let mut s = String::new();
        render(&mut s);
        assert!(s.contains("pairing_approvals_total{channel=\"whatsapp\",result=\"ok\"} 1"));
        assert!(s.contains("pairing_codes_expired_total 4"));
        assert!(s.contains("pairing_bootstrap_tokens_issued_total{profile=\"default\"} 1"));
        assert!(s.contains("pairing_requests_pending{channel=\"telegram\"} 2"));
    }
}
