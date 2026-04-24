use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

/// Histogram buckets in milliseconds.
const LATENCY_BUCKET_LIMITS_MS: [u64; 8] = [50, 100, 250, 500, 1000, 2500, 5000, 10000];

/// Labels attached to every LLM request/latency sample.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct LlmKey {
    agent: String,
    provider: String,
    model: String,
}

/// Per-series histogram backing store.
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
        for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
            if duration_ms <= *upper {
                self.buckets[idx].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

static LLM_REQUESTS: LazyLock<DashMap<LlmKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LLM_LATENCY: LazyLock<DashMap<LlmKey, Histogram>> = LazyLock::new(DashMap::new);
static MESSAGES_PROCESSED: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
static CIRCUIT_BREAKER_STATE: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);

/// Phase 9.2 follow-up — per-tool call counter with outcome label.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ToolCallKey {
    agent: String,
    tool: String,
    outcome: String,
}

/// Per-tool latency histogram key (no outcome — cardinality control).
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ToolKey {
    agent: String,
    tool: String,
}

static TOOL_CALLS: LazyLock<DashMap<ToolCallKey, AtomicU64>> = LazyLock::new(DashMap::new);
static TOOL_LATENCY: LazyLock<DashMap<ToolKey, Histogram>> = LazyLock::new(DashMap::new);
/// Per-(agent, tool, event) cache counters. `event` is one of
/// `"hit" | "miss" | "put" | "evict"`. Lets ops measure cache hit
/// rate without instrumenting each handler by hand.
static TOOL_CACHE_EVENTS: LazyLock<DashMap<ToolCallKey, AtomicU64>> =
    LazyLock::new(DashMap::new);

pub fn inc_messages_processed_total(agent_id: &str) {
    MESSAGES_PROCESSED
        .entry(agent_id.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn inc_llm_requests_total(agent_id: &str, provider: &str, model: &str) {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_REQUESTS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

pub fn observe_llm_latency_ms(agent_id: &str, provider: &str, model: &str, duration_ms: u64) {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_LATENCY
        .entry(key)
        .or_insert_with(Histogram::new)
        .observe(duration_ms);
}

/// Phase 9.2 follow-up — bump the per-tool call counter. `outcome` is
/// one of `"ok" | "error" | "blocked" | "unknown"` per convention.
pub fn inc_tool_calls_total(agent: &str, tool: &str, outcome: &str) {
    let key = ToolCallKey {
        agent: agent.to_string(),
        tool: tool.to_string(),
        outcome: outcome.to_string(),
    };
    TOOL_CALLS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Phase 9.2 follow-up — record tool handler latency in milliseconds.
/// Label set is `(agent, tool)`; outcome is intentionally omitted to
/// avoid cardinality blow-up.
pub fn observe_tool_latency_ms(agent: &str, tool: &str, duration_ms: u64) {
    let key = ToolKey {
        agent: agent.to_string(),
        tool: tool.to_string(),
    };
    TOOL_LATENCY
        .entry(key)
        .or_insert_with(Histogram::new)
        .observe(duration_ms);
}

/// Tool cache observability. `event` ∈ {`"hit"`, `"miss"`, `"put"`,
/// `"evict"`}. Hit rate is `hit / (hit + miss)`.
pub fn inc_tool_cache_event(agent: &str, tool: &str, event: &str) {
    let key = ToolCallKey {
        agent: agent.to_string(),
        tool: tool.to_string(),
        outcome: event.to_string(),
    };
    TOOL_CACHE_EVENTS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Update circuit breaker state gauge. `open=true` → 1, `open=false` → 0.
pub fn set_circuit_breaker_state(breaker: &str, open: bool) {
    CIRCUIT_BREAKER_STATE
        .entry(breaker.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .store(if open { 1 } else { 0 }, Ordering::Relaxed);
}

/// Reset every metric — intended for test isolation only.
#[cfg(test)]
pub fn reset_for_test() {
    LLM_REQUESTS.clear();
    LLM_LATENCY.clear();
    MESSAGES_PROCESSED.clear();
    CIRCUIT_BREAKER_STATE.clear();
    TOOL_CALLS.clear();
    TOOL_LATENCY.clear();
    TOOL_CACHE_EVENTS.clear();
}

pub fn render_prometheus(fallback_nats_open: bool) -> String {
    // Ensure the nats circuit breaker always appears even if no explicit update has fired yet.
    if !CIRCUIT_BREAKER_STATE.contains_key("nats") {
        set_circuit_breaker_state("nats", fallback_nats_open);
    }

    let mut out = String::new();

    out.push_str("# HELP llm_requests_total Total number of LLM chat requests.\n");
    out.push_str("# TYPE llm_requests_total counter\n");
    if LLM_REQUESTS.is_empty() {
        out.push_str("llm_requests_total 0\n");
    } else {
        let mut rows: Vec<_> = LLM_REQUESTS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| (a.0.agent.clone(), a.0.provider.clone(), a.0.model.clone())
            .cmp(&(b.0.agent.clone(), b.0.provider.clone(), b.0.model.clone())));
        for (key, v) in rows {
            out.push_str(&format!(
                "llm_requests_total{{agent=\"{}\",provider=\"{}\",model=\"{}\"}} {}\n",
                escape(&key.agent),
                escape(&key.provider),
                escape(&key.model),
                v
            ));
        }
    }

    out.push_str("# HELP llm_latency_ms LLM request latency histogram in milliseconds.\n");
    out.push_str("# TYPE llm_latency_ms histogram\n");
    if LLM_LATENCY.is_empty() {
        // Emit an empty series with default labels so scrapers do not report a missing metric.
        for upper in LATENCY_BUCKET_LIMITS_MS.iter() {
            out.push_str(&format!(
                "llm_latency_ms_bucket{{agent=\"\",provider=\"\",model=\"\",le=\"{upper}\"}} 0\n"
            ));
        }
        out.push_str("llm_latency_ms_bucket{agent=\"\",provider=\"\",model=\"\",le=\"+Inf\"} 0\n");
        out.push_str("llm_latency_ms_sum{agent=\"\",provider=\"\",model=\"\"} 0\n");
        out.push_str("llm_latency_ms_count{agent=\"\",provider=\"\",model=\"\"} 0\n");
    } else {
        let keys: Vec<LlmKey> = {
            let mut ks: Vec<_> = LLM_LATENCY.iter().map(|e| e.key().clone()).collect();
            ks.sort_by(|a, b| (a.agent.clone(), a.provider.clone(), a.model.clone())
                .cmp(&(b.agent.clone(), b.provider.clone(), b.model.clone())));
            ks
        };
        for key in keys {
            let Some(series) = LLM_LATENCY.get(&key) else { continue };
            let agent = escape(&key.agent);
            let provider = escape(&key.provider);
            let model = escape(&key.model);
            for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "llm_latency_ms_bucket{{agent=\"{agent}\",provider=\"{provider}\",model=\"{model}\",le=\"{upper}\"}} {}\n",
                    series.buckets[idx].load(Ordering::Relaxed)
                ));
            }
            let count = series.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "llm_latency_ms_bucket{{agent=\"{agent}\",provider=\"{provider}\",model=\"{model}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "llm_latency_ms_sum{{agent=\"{agent}\",provider=\"{provider}\",model=\"{model}\"}} {}\n",
                series.sum_ms.load(Ordering::Relaxed)
            ));
            out.push_str(&format!(
                "llm_latency_ms_count{{agent=\"{agent}\",provider=\"{provider}\",model=\"{model}\"}} {count}\n"
            ));
        }
    }

    out.push_str("# HELP messages_processed_total Total number of processed inbound messages.\n");
    out.push_str("# TYPE messages_processed_total counter\n");
    if MESSAGES_PROCESSED.is_empty() {
        out.push_str("messages_processed_total{agent=\"\"} 0\n");
    } else {
        let mut rows: Vec<_> = MESSAGES_PROCESSED
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (agent, v) in rows {
            out.push_str(&format!(
                "messages_processed_total{{agent=\"{}\"}} {}\n",
                escape(&agent),
                v
            ));
        }
    }

    // Phase 9.2 follow-up — per-tool counter.
    out.push_str("# HELP agent_tool_calls_total Total tool invocations from the LLM loop.\n");
    out.push_str("# TYPE agent_tool_calls_total counter\n");
    if TOOL_CALLS.is_empty() {
        out.push_str("agent_tool_calls_total 0\n");
    } else {
        let mut rows: Vec<_> = TOOL_CALLS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.agent.clone(), a.0.outcome.clone(), a.0.tool.clone())
                .cmp(&(b.0.agent.clone(), b.0.outcome.clone(), b.0.tool.clone()))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "agent_tool_calls_total{{agent=\"{}\",outcome=\"{}\",tool=\"{}\"}} {}\n",
                escape(&key.agent),
                escape(&key.outcome),
                escape(&key.tool),
                v
            ));
        }
    }

    // Tool policy cache observability.
    out.push_str(
        "# HELP agent_tool_cache_events_total Tool cache events by agent/tool/event.\n",
    );
    out.push_str("# TYPE agent_tool_cache_events_total counter\n");
    if TOOL_CACHE_EVENTS.is_empty() {
        out.push_str("agent_tool_cache_events_total 0\n");
    } else {
        let mut rows: Vec<_> = TOOL_CACHE_EVENTS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.agent.clone(), a.0.outcome.clone(), a.0.tool.clone())
                .cmp(&(b.0.agent.clone(), b.0.outcome.clone(), b.0.tool.clone()))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "agent_tool_cache_events_total{{agent=\"{}\",event=\"{}\",tool=\"{}\"}} {}\n",
                escape(&key.agent),
                escape(&key.outcome),
                escape(&key.tool),
                v
            ));
        }
    }

    // Phase 9.2 follow-up — per-tool latency histogram.
    out.push_str("# HELP agent_tool_latency_ms Tool handler latency histogram in milliseconds.\n");
    out.push_str("# TYPE agent_tool_latency_ms histogram\n");
    if TOOL_LATENCY.is_empty() {
        for upper in LATENCY_BUCKET_LIMITS_MS.iter() {
            out.push_str(&format!(
                "agent_tool_latency_ms_bucket{{agent=\"\",tool=\"\",le=\"{upper}\"}} 0\n"
            ));
        }
        out.push_str("agent_tool_latency_ms_bucket{agent=\"\",tool=\"\",le=\"+Inf\"} 0\n");
        out.push_str("agent_tool_latency_ms_sum{agent=\"\",tool=\"\"} 0\n");
        out.push_str("agent_tool_latency_ms_count{agent=\"\",tool=\"\"} 0\n");
    } else {
        let keys: Vec<ToolKey> = {
            let mut ks: Vec<_> = TOOL_LATENCY.iter().map(|e| e.key().clone()).collect();
            ks.sort_by(|a, b| (a.agent.clone(), a.tool.clone()).cmp(&(b.agent.clone(), b.tool.clone())));
            ks
        };
        for key in keys {
            let Some(series) = TOOL_LATENCY.get(&key) else { continue };
            let agent = escape(&key.agent);
            let tool = escape(&key.tool);
            for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "agent_tool_latency_ms_bucket{{agent=\"{agent}\",tool=\"{tool}\",le=\"{upper}\"}} {}\n",
                    series.buckets[idx].load(Ordering::Relaxed)
                ));
            }
            let count = series.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "agent_tool_latency_ms_bucket{{agent=\"{agent}\",tool=\"{tool}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "agent_tool_latency_ms_sum{{agent=\"{agent}\",tool=\"{tool}\"}} {}\n",
                series.sum_ms.load(Ordering::Relaxed)
            ));
            out.push_str(&format!(
                "agent_tool_latency_ms_count{{agent=\"{agent}\",tool=\"{tool}\"}} {count}\n"
            ));
        }
    }

    out.push_str("# HELP circuit_breaker_state Circuit breaker state (0=closed,1=open).\n");
    out.push_str("# TYPE circuit_breaker_state gauge\n");
    let mut rows: Vec<_> = CIRCUIT_BREAKER_STATE
        .iter()
        .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (breaker, v) in rows {
        out.push_str(&format!(
            "circuit_breaker_state{{breaker=\"{}\"}} {}\n",
            escape(&breaker),
            v
        ));
    }

    out
}

/// Escape a string for use as a Prometheus label value (RFC: backslash, quote, newline).
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests touch the global metric registry — serialize them to avoid cross-test pollution.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn contains_line(body: &str, prefix: &str) -> bool {
        body.lines().any(|l| l.starts_with(prefix))
    }

    #[test]
    fn tool_metrics_render_correctly() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_tool_calls_total("kate", "mcp_fs_read", "ok");
        inc_tool_calls_total("kate", "mcp_fs_read", "ok");
        inc_tool_calls_total("kate", "mcp_fs_read", "error");
        observe_tool_latency_ms("kate", "mcp_fs_read", 42);
        observe_tool_latency_ms("kate", "mcp_fs_read", 800);
        let body = render_prometheus(false);
        assert!(
            contains_line(
                &body,
                "agent_tool_calls_total{agent=\"kate\",outcome=\"ok\",tool=\"mcp_fs_read\"} 2"
            ),
            "missing ok counter:\n{body}"
        );
        assert!(contains_line(
            &body,
            "agent_tool_calls_total{agent=\"kate\",outcome=\"error\",tool=\"mcp_fs_read\"} 1"
        ));
        assert!(contains_line(
            &body,
            "agent_tool_latency_ms_count{agent=\"kate\",tool=\"mcp_fs_read\"} 2"
        ));
        assert!(
            body.contains("agent_tool_latency_ms_bucket{agent=\"kate\",tool=\"mcp_fs_read\",le=\"50\"}"),
            "missing 50ms bucket:\n{body}"
        );
    }

    #[test]
    fn renders_default_zero_series_when_empty() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let body = render_prometheus(false);
        assert!(contains_line(&body, "llm_requests_total 0"));
        assert!(contains_line(
            &body,
            "messages_processed_total{agent=\"\"} 0"
        ));
        assert!(contains_line(
            &body,
            "circuit_breaker_state{breaker=\"nats\"}"
        ));
    }

    #[test]
    fn llm_labels_are_emitted() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_llm_requests_total("kate", "minimax", "MiniMax-Text-01");
        observe_llm_latency_ms("kate", "minimax", "MiniMax-Text-01", 42);

        let body = render_prometheus(false);
        assert!(body.contains(
            "llm_requests_total{agent=\"kate\",provider=\"minimax\",model=\"MiniMax-Text-01\"} 1"
        ));
        assert!(body.contains(
            "llm_latency_ms_count{agent=\"kate\",provider=\"minimax\",model=\"MiniMax-Text-01\"} 1"
        ));
        assert!(body.contains("\"le=\\\"50\\\"".to_string().as_str())
            || body.contains("le=\"50\""));
        assert!(body.contains("llm_latency_ms_sum{agent=\"kate\",provider=\"minimax\",model=\"MiniMax-Text-01\"} 42"));
    }

    #[test]
    fn messages_labels_per_agent() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_messages_processed_total("kate");
        inc_messages_processed_total("kate");
        inc_messages_processed_total("mori");

        let body = render_prometheus(false);
        assert!(body.contains("messages_processed_total{agent=\"kate\"} 2"));
        assert!(body.contains("messages_processed_total{agent=\"mori\"} 1"));
    }

    #[test]
    fn circuit_breaker_gauge_tracks_multiple_breakers() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        set_circuit_breaker_state("nats", true);
        set_circuit_breaker_state("browser", false);

        let body = render_prometheus(false);
        assert!(body.contains("circuit_breaker_state{breaker=\"nats\"} 1"));
        assert!(body.contains("circuit_breaker_state{breaker=\"browser\"} 0"));
    }

    #[test]
    fn label_values_are_escaped() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        inc_llm_requests_total("weird\"agent", "prov\\1", "m\nodel");
        let body = render_prometheus(false);
        assert!(body.contains("agent=\"weird\\\"agent\""));
        assert!(body.contains("provider=\"prov\\\\1\""));
        assert!(body.contains("model=\"m\\nodel\""));
    }
}
