//! Streaming telemetry for `nexo-llm`.
//!
//! Lives in `nexo-llm` so parser/client code can emit metrics without
//! depending on `nexo-core` (which would create a circular dependency).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use dashmap::DashMap;

const TTFT_BUCKET_LIMITS_MS: [u64; 8] = [100, 250, 500, 1000, 2000, 5000, 10000, 30000];

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ChunkKey {
    provider: String,
    kind: String,
}

struct Histogram {
    buckets: [AtomicU64; 8],
    count: AtomicU64,
    sum_ms: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_ms: AtomicU64::new(0),
        }
    }

    fn observe(&self, duration_ms: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ms.fetch_add(duration_ms, Ordering::Relaxed);
        for (idx, upper) in TTFT_BUCKET_LIMITS_MS.iter().enumerate() {
            if duration_ms <= *upper {
                self.buckets[idx].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

static STREAM_TTFT: LazyLock<DashMap<String, Histogram>> = LazyLock::new(DashMap::new);
static STREAM_CHUNKS: LazyLock<DashMap<ChunkKey, AtomicU64>> = LazyLock::new(DashMap::new);

pub fn observe_stream_ttft_ms(provider: &str, duration_ms: u64) {
    STREAM_TTFT
        .entry(provider.to_string())
        .or_insert_with(Histogram::new)
        .observe(duration_ms);
}

pub fn inc_stream_chunks_total(provider: &str, kind: &str) {
    STREAM_CHUNKS
        .entry(ChunkKey {
            provider: provider.to_string(),
            kind: kind.to_string(),
        })
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
pub fn reset_for_test() {
    STREAM_TTFT.clear();
    STREAM_CHUNKS.clear();
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

fn fmt_secs(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

pub fn render_prometheus() -> String {
    render_into(&STREAM_TTFT, &STREAM_CHUNKS)
}

/// Pure renderer — takes the telemetry maps explicitly so tests can
/// pass their own isolated `DashMap`s and never touch the global
/// static. Production callers go through `render_prometheus()` which
/// reads the global. Phase 38.x.1 fix.
fn render_into(
    ttft: &DashMap<String, Histogram>,
    chunks: &DashMap<ChunkKey, AtomicU64>,
) -> String {
    let mut out = String::new();

    out.push_str(
        "# HELP nexo_llm_stream_ttft_seconds Time-to-first-token for streaming responses.\n",
    );
    out.push_str("# TYPE nexo_llm_stream_ttft_seconds histogram\n");
    if ttft.is_empty() {
        for upper in TTFT_BUCKET_LIMITS_MS.iter() {
            out.push_str(&format!(
                "nexo_llm_stream_ttft_seconds_bucket{{provider=\"\",le=\"{}\"}} 0\n",
                fmt_secs(*upper)
            ));
        }
        out.push_str("nexo_llm_stream_ttft_seconds_bucket{provider=\"\",le=\"+Inf\"} 0\n");
        out.push_str("nexo_llm_stream_ttft_seconds_sum{provider=\"\"} 0\n");
        out.push_str("nexo_llm_stream_ttft_seconds_count{provider=\"\"} 0\n");
    } else {
        let mut providers: Vec<String> = ttft.iter().map(|e| e.key().clone()).collect();
        providers.sort();
        providers.dedup();
        for provider in providers {
            let Some(series) = ttft.get(&provider) else {
                continue;
            };
            let provider = escape(&provider);
            for (idx, upper) in TTFT_BUCKET_LIMITS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "nexo_llm_stream_ttft_seconds_bucket{{provider=\"{provider}\",le=\"{}\"}} {}\n",
                    fmt_secs(*upper),
                    series.buckets[idx].load(Ordering::Relaxed)
                ));
            }
            let count = series.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "nexo_llm_stream_ttft_seconds_bucket{{provider=\"{provider}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "nexo_llm_stream_ttft_seconds_sum{{provider=\"{provider}\"}} {}\n",
                fmt_secs(series.sum_ms.load(Ordering::Relaxed))
            ));
            out.push_str(&format!(
                "nexo_llm_stream_ttft_seconds_count{{provider=\"{provider}\"}} {count}\n"
            ));
        }
    }

    out.push_str(
        "# HELP nexo_llm_stream_chunks_total Streaming chunks emitted by provider/kind.\n",
    );
    out.push_str("# TYPE nexo_llm_stream_chunks_total counter\n");
    if chunks.is_empty() {
        out.push_str("nexo_llm_stream_chunks_total{provider=\"\",kind=\"\"} 0\n");
    } else {
        let mut rows: Vec<_> = chunks
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.provider.clone(), a.0.kind.clone()).cmp(&(b.0.provider.clone(), b.0.kind.clone()))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "nexo_llm_stream_chunks_total{{provider=\"{}\",kind=\"{}\"}} {}\n",
                escape(&key.provider),
                escape(&key.kind),
                v
            ));
        }
    }

    out
}

/// Serializes every test that touches the global telemetry state
/// (telemetry::tests + stream::tests both reset / observe / render).
/// Without this, parallel tests trip over `reset_for_test` clearing
/// the maps mid-render and one Mutex panic poisons the rest.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_line(haystack: &str, needle: &str) -> bool {
        haystack.lines().any(|line| line == needle)
    }

    #[test]
    fn render_stream_metrics_and_escaping() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        observe_stream_ttft_ms("openai", 120);
        observe_stream_ttft_ms("openai", 920);
        inc_stream_chunks_total("openai", "text_delta");
        inc_stream_chunks_total("openai", "usage");
        inc_stream_chunks_total("prov\"x", "line\nbreak");

        let body = render_prometheus();
        assert!(contains_line(
            &body,
            "nexo_llm_stream_ttft_seconds_count{provider=\"openai\"} 2"
        ));
        assert!(contains_line(
            &body,
            "nexo_llm_stream_chunks_total{provider=\"openai\",kind=\"text_delta\"} 1"
        ));
        assert!(contains_line(
            &body,
            "nexo_llm_stream_chunks_total{provider=\"openai\",kind=\"usage\"} 1"
        ));
        assert!(contains_line(
            &body,
            "nexo_llm_stream_chunks_total{provider=\"prov\\\"x\",kind=\"line\\nbreak\"} 1"
        ));
    }

    // Phase 38.x.1 fix: render against a local registry instead of
    // the shared global. No `TEST_LOCK`, no `reset_for_test`, no race
    // with sibling crate tests under `cargo test --workspace`.
    #[test]
    fn render_empty_series_when_no_samples() {
        let ttft: DashMap<String, Histogram> = DashMap::new();
        let chunks: DashMap<ChunkKey, AtomicU64> = DashMap::new();
        let body = render_into(&ttft, &chunks);
        assert!(contains_line(
            &body,
            "nexo_llm_stream_ttft_seconds_count{provider=\"\"} 0"
        ));
        assert!(contains_line(
            &body,
            "nexo_llm_stream_chunks_total{provider=\"\",kind=\"\"} 0"
        ));
    }

    // Same isolation pattern for a populated render — exercises the
    // non-empty branch without ever touching the globals.
    #[test]
    fn render_into_local_registry_with_samples() {
        let ttft: DashMap<String, Histogram> = DashMap::new();
        let chunks: DashMap<ChunkKey, AtomicU64> = DashMap::new();
        ttft.entry("openai".to_string())
            .or_insert_with(Histogram::new)
            .observe(120);
        chunks
            .entry(ChunkKey {
                provider: "openai".into(),
                kind: "text_delta".into(),
            })
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);

        let body = render_into(&ttft, &chunks);
        assert!(contains_line(
            &body,
            "nexo_llm_stream_ttft_seconds_count{provider=\"openai\"} 1"
        ));
        assert!(contains_line(
            &body,
            "nexo_llm_stream_chunks_total{provider=\"openai\",kind=\"text_delta\"} 1"
        ));
    }
}
