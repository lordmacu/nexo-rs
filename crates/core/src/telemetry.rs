use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

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
static TOOL_CACHE_EVENTS: LazyLock<DashMap<ToolCallKey, AtomicU64>> = LazyLock::new(DashMap::new);

/// Phase B — compaction outcome counters per (agent, outcome). Outcomes:
/// `"ok"`, `"failed"`, `"lock_held"`, `"no_boundary"`,
/// `"tool_result_truncated"`. Histogram on duration is keyed by
/// (agent, outcome="ok"|"failed") only — lock_held / no_boundary cases
/// never run the LLM, so 0 ms entries would skew the percentiles.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct CompactionKey {
    agent: String,
    outcome: String,
}
static COMPACTION_TOTAL: LazyLock<DashMap<CompactionKey, AtomicU64>> = LazyLock::new(DashMap::new);
static COMPACTION_DURATION: LazyLock<DashMap<CompactionKey, Histogram>> =
    LazyLock::new(DashMap::new);

/// Bump the counter and (when meaningful) record duration for a
/// compaction outcome.
pub fn observe_compaction(agent_id: &str, outcome: &str, duration_ms: u64) {
    let key = CompactionKey {
        agent: agent_id.to_string(),
        outcome: outcome.to_string(),
    };
    COMPACTION_TOTAL
        .entry(key.clone())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
    if matches!(outcome, "ok" | "failed") && duration_ms > 0 {
        COMPACTION_DURATION
            .entry(key)
            .or_insert_with(Histogram::new)
            .observe(duration_ms);
    }
}

/// Test helper: read a compaction counter for a given outcome.
pub fn compaction_total(agent_id: &str, outcome: &str) -> u64 {
    let key = CompactionKey {
        agent: agent_id.to_string(),
        outcome: outcome.to_string(),
    };
    COMPACTION_TOTAL
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Phase A.2 — provider-level prompt cache counters per (agent, provider).
/// Field meaning matches `nexo_llm::CacheUsage`. The model is part of
/// the key so a binding-driven model swap shows up as a fresh hit-ratio
/// curve in dashboards.
static LLM_CACHE_READ: LazyLock<DashMap<LlmKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LLM_CACHE_CREATION: LazyLock<DashMap<LlmKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LLM_CACHE_INPUT: LazyLock<DashMap<LlmKey, AtomicU64>> = LazyLock::new(DashMap::new);
static LLM_CACHE_TURNS: LazyLock<DashMap<LlmKey, AtomicU64>> = LazyLock::new(DashMap::new);
/// Phase 11.2 follow-up — extension discovery counters by status.
/// `status` is one of `"ok" | "disabled" | "invalid"`.
static EXTENSIONS_DISCOVERED: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);

/// Phase 26.x — pairing inbound challenge delivery outcomes per
/// `(channel, result)`. `result` is one of `"delivered_via_adapter"
/// | "delivered_via_broker" | "publish_failed"
/// | "no_adapter_no_broker_topic"`. Lets ops watch how often the
/// adapter path is exercised vs. the legacy hardcoded broker fallback,
/// and surfaces failure rates per channel.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct PairingChallengeKey {
    channel: String,
    result: String,
}
static PAIRING_INBOUND_CHALLENGED: LazyLock<DashMap<PairingChallengeKey, AtomicU64>> =
    LazyLock::new(DashMap::new);

/// Bump the pairing-challenge counter for a `(channel, result)` pair.
pub fn inc_pairing_inbound_challenged(channel: &str, result: &str) {
    let key = PairingChallengeKey {
        channel: channel.to_string(),
        result: result.to_string(),
    };
    PAIRING_INBOUND_CHALLENGED
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Test helper — read a pairing-challenge counter for a `(channel,
/// result)` pair.
pub fn pairing_inbound_challenged_total(channel: &str, result: &str) -> u64 {
    let key = PairingChallengeKey {
        channel: channel.to_string(),
        result: result.to_string(),
    };
    PAIRING_INBOUND_CHALLENGED
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Phase 21 follow-up (L-1) — link-understanding fetch outcome counter.
/// `result` is one of `"ok" | "blocked" | "timeout" | "non_html" | "too_big" | "error"`.
static LINK_FETCH_TOTAL: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
/// Phase 21 follow-up (L-1) — cache-hit counter. `hit` is `"true" | "false"`.
static LINK_CACHE_TOTAL: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);
/// Phase 21 follow-up (L-1) — per-fetch latency histogram (single series,
/// no labels — keeps cardinality flat; the result counter already
/// segments outcomes).
static LINK_FETCH_DURATION: LazyLock<Histogram> = LazyLock::new(Histogram::new);

/// Bump the link-understanding fetch counter for `result`. Call from
/// the extractor on every HTTP attempt (cache hits use
/// [`inc_link_cache`] instead).
pub fn inc_link_fetch(result: &str) {
    LINK_FETCH_TOTAL
        .entry(result.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Bump the link-understanding cache counter. `hit=true` for a TTL-fresh
/// cache return, `hit=false` for a miss (about to fetch).
pub fn inc_link_cache(hit: bool) {
    let label = if hit { "true" } else { "false" };
    LINK_CACHE_TOTAL
        .entry(label.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Observe a link-understanding fetch duration in milliseconds.
/// Recorded for every fetch attempt that actually issued the HTTP
/// request — cache hits and host-blocked URLs never reach the
/// network and would distort the histogram.
pub fn observe_link_fetch_ms(duration_ms: u64) {
    LINK_FETCH_DURATION.observe(duration_ms);
}

/// Test helper — read a fetch counter for a given outcome.
pub fn link_fetch_total(result: &str) -> u64 {
    LINK_FETCH_TOTAL
        .get(result)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Test helper — read a cache counter for a given hit value.
pub fn link_cache_total(hit: bool) -> u64 {
    let label = if hit { "true" } else { "false" };
    LINK_CACHE_TOTAL
        .get(label)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

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

/// Phase A.2 — accumulate prompt-cache token counters for the
/// `(agent, provider, model)` key. `cache_read_input_tokens` and
/// `cache_creation_input_tokens` add to running counters; `input_tokens`
/// (uncached prompt) joins the same totals so a downstream dashboard
/// can compute hit ratio = read / (read + creation + input).
pub fn observe_cache_usage(
    agent_id: &str,
    provider: &str,
    model: &str,
    usage: &nexo_llm::CacheUsage,
) {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_CACHE_READ
        .entry(key.clone())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(usage.cache_read_input_tokens as u64, Ordering::Relaxed);
    LLM_CACHE_CREATION
        .entry(key.clone())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(usage.cache_creation_input_tokens as u64, Ordering::Relaxed);
    LLM_CACHE_INPUT
        .entry(key.clone())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(usage.input_tokens as u64, Ordering::Relaxed);
    LLM_CACHE_TURNS
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

/// Phase C — pre-flight estimated input tokens (last sample per key).
/// Stored as last-value gauge; the rolling history lives in the
/// drift histogram.
static LLM_PROMPT_TOKENS_ESTIMATED: LazyLock<DashMap<LlmKey, AtomicU64>> =
    LazyLock::new(DashMap::new);
/// Phase C — drift between pre-flight estimate and provider-reported
/// actual prompt_tokens, in **percent** (`abs(est - actual) / actual *
/// 100`). Histogram so dashboards can alert on p99 drift instead of
/// average smoothing it away.
static LLM_PROMPT_TOKENS_DRIFT_PCT: LazyLock<DashMap<LlmKey, Histogram>> =
    LazyLock::new(DashMap::new);

/// Phase C — record the pre-flight token estimate for a request. The
/// `exact` flag should reflect `TokenCounter::is_exact()` so dashboards
/// can group exact vs approximate samples.
pub fn observe_prompt_tokens_estimated(
    agent_id: &str,
    provider: &str,
    model: &str,
    estimated: u32,
    _exact: bool,
) {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_PROMPT_TOKENS_ESTIMATED
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .store(estimated as u64, Ordering::Relaxed);
}

/// Phase C — record the drift between estimate and actual after a
/// request returned. `actual` should be the provider-reported total
/// prompt tokens (cached read + cached creation + uncached input).
/// No-op when `actual == 0` (provider didn't report usage).
pub fn observe_prompt_tokens_drift(
    agent_id: &str,
    provider: &str,
    model: &str,
    estimated: u32,
    actual: u32,
) {
    if actual == 0 {
        return;
    }
    let diff = estimated.abs_diff(actual);
    let pct = ((diff as f64) * 100.0 / (actual as f64)).round() as u64;
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_PROMPT_TOKENS_DRIFT_PCT
        .entry(key)
        .or_insert_with(Histogram::new)
        .observe(pct);
}

/// Test helper: read the last estimated-tokens gauge sample.
pub fn prompt_tokens_estimated(agent_id: &str, provider: &str, model: &str) -> u64 {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_PROMPT_TOKENS_ESTIMATED
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Test helper: rolling drift histogram count (number of observations).
pub fn prompt_tokens_drift_observations(agent_id: &str, provider: &str, model: &str) -> u64 {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    LLM_PROMPT_TOKENS_DRIFT_PCT
        .get(&key)
        .map(|v| v.value().count.load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// Snapshot of prompt-cache counters for a given key. Returns
/// `(read, creation, input, turns)`. Used by tests and the admin
/// surface to inspect rolling hit ratios.
pub fn cache_usage_totals(agent_id: &str, provider: &str, model: &str) -> (u64, u64, u64, u64) {
    let key = LlmKey {
        agent: agent_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
    };
    let read = LLM_CACHE_READ
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0);
    let creation = LLM_CACHE_CREATION
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0);
    let input = LLM_CACHE_INPUT
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0);
    let turns = LLM_CACHE_TURNS
        .get(&key)
        .map(|v| v.value().load(Ordering::Relaxed))
        .unwrap_or(0);
    (read, creation, input, turns)
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

/// Phase 11.2 follow-up — add N extension-discovery results under one
/// status label. This is called once per discovery pass at startup.
pub fn add_extensions_discovered(status: &str, count: u64) {
    if count == 0 {
        return;
    }
    EXTENSIONS_DISCOVERED
        .entry(status.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(count, Ordering::Relaxed);
}

/// Phase 18 — hot-reload telemetry. Tracks applied / rejected reload
/// attempts + the monotonic config version per agent so operators can
/// correlate which snapshot was active when a session fired.
static CONFIG_RELOAD_APPLIED: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static CONFIG_RELOAD_REJECTED: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
static CONFIG_RELOAD_LATENCY: LazyLock<Histogram> = LazyLock::new(Histogram::new);
static RUNTIME_CONFIG_VERSION: LazyLock<DashMap<String, AtomicU64>> = LazyLock::new(DashMap::new);

pub fn inc_config_reload_applied() {
    CONFIG_RELOAD_APPLIED.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_config_reload_rejected() {
    CONFIG_RELOAD_REJECTED.fetch_add(1, Ordering::Relaxed);
}

pub fn observe_config_reload_latency_ms(duration_ms: u64) {
    CONFIG_RELOAD_LATENCY.observe(duration_ms);
}

pub fn set_runtime_config_version(agent_id: &str, version: u64) {
    RUNTIME_CONFIG_VERSION
        .entry(agent_id.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .store(version, Ordering::Relaxed);
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
    EXTENSIONS_DISCOVERED.clear();
    LINK_FETCH_TOTAL.clear();
    LINK_CACHE_TOTAL.clear();
    PAIRING_INBOUND_CHALLENGED.clear();
    CONFIG_RELOAD_APPLIED.store(0, Ordering::Relaxed);
    CONFIG_RELOAD_REJECTED.store(0, Ordering::Relaxed);
    RUNTIME_CONFIG_VERSION.clear();
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
        rows.sort_by(|a, b| {
            (a.0.agent.clone(), a.0.provider.clone(), a.0.model.clone()).cmp(&(
                b.0.agent.clone(),
                b.0.provider.clone(),
                b.0.model.clone(),
            ))
        });
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
            ks.sort_by(|a, b| {
                (a.agent.clone(), a.provider.clone(), a.model.clone()).cmp(&(
                    b.agent.clone(),
                    b.provider.clone(),
                    b.model.clone(),
                ))
            });
            ks
        };
        for key in keys {
            let Some(series) = LLM_LATENCY.get(&key) else {
                continue;
            };
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

    out.push_str(
        "# HELP nexo_extensions_discovered Total extension discovery outcomes by status.\n",
    );
    out.push_str("# TYPE nexo_extensions_discovered counter\n");
    for status in ["ok", "disabled", "invalid"] {
        let v = EXTENSIONS_DISCOVERED
            .get(status)
            .map(|x| x.load(Ordering::Relaxed))
            .unwrap_or(0);
        out.push_str(&format!(
            "nexo_extensions_discovered{{status=\"{status}\"}} {v}\n"
        ));
    }

    // Phase 9.2 follow-up — per-tool counter.
    out.push_str("# HELP nexo_tool_calls_total Total tool invocations from the LLM loop.\n");
    out.push_str("# TYPE nexo_tool_calls_total counter\n");
    if TOOL_CALLS.is_empty() {
        out.push_str("nexo_tool_calls_total 0\n");
    } else {
        let mut rows: Vec<_> = TOOL_CALLS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.agent.clone(), a.0.outcome.clone(), a.0.tool.clone()).cmp(&(
                b.0.agent.clone(),
                b.0.outcome.clone(),
                b.0.tool.clone(),
            ))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "nexo_tool_calls_total{{agent=\"{}\",outcome=\"{}\",tool=\"{}\"}} {}\n",
                escape(&key.agent),
                escape(&key.outcome),
                escape(&key.tool),
                v
            ));
        }
    }

    // Tool policy cache observability.
    out.push_str("# HELP nexo_tool_cache_events_total Tool cache events by agent/tool/event.\n");
    out.push_str("# TYPE nexo_tool_cache_events_total counter\n");
    if TOOL_CACHE_EVENTS.is_empty() {
        out.push_str("nexo_tool_cache_events_total 0\n");
    } else {
        let mut rows: Vec<_> = TOOL_CACHE_EVENTS
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.agent.clone(), a.0.outcome.clone(), a.0.tool.clone()).cmp(&(
                b.0.agent.clone(),
                b.0.outcome.clone(),
                b.0.tool.clone(),
            ))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "nexo_tool_cache_events_total{{agent=\"{}\",event=\"{}\",tool=\"{}\"}} {}\n",
                escape(&key.agent),
                escape(&key.outcome),
                escape(&key.tool),
                v
            ));
        }
    }

    // Phase 9.2 follow-up — per-tool latency histogram.
    out.push_str("# HELP nexo_tool_latency_ms Tool handler latency histogram in milliseconds.\n");
    out.push_str("# TYPE nexo_tool_latency_ms histogram\n");
    if TOOL_LATENCY.is_empty() {
        for upper in LATENCY_BUCKET_LIMITS_MS.iter() {
            out.push_str(&format!(
                "nexo_tool_latency_ms_bucket{{agent=\"\",tool=\"\",le=\"{upper}\"}} 0\n"
            ));
        }
        out.push_str("nexo_tool_latency_ms_bucket{agent=\"\",tool=\"\",le=\"+Inf\"} 0\n");
        out.push_str("nexo_tool_latency_ms_sum{agent=\"\",tool=\"\"} 0\n");
        out.push_str("nexo_tool_latency_ms_count{agent=\"\",tool=\"\"} 0\n");
    } else {
        let keys: Vec<ToolKey> = {
            let mut ks: Vec<_> = TOOL_LATENCY.iter().map(|e| e.key().clone()).collect();
            ks.sort_by(|a, b| {
                (a.agent.clone(), a.tool.clone()).cmp(&(b.agent.clone(), b.tool.clone()))
            });
            ks
        };
        for key in keys {
            let Some(series) = TOOL_LATENCY.get(&key) else {
                continue;
            };
            let agent = escape(&key.agent);
            let tool = escape(&key.tool);
            for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
                out.push_str(&format!(
                    "nexo_tool_latency_ms_bucket{{agent=\"{agent}\",tool=\"{tool}\",le=\"{upper}\"}} {}\n",
                    series.buckets[idx].load(Ordering::Relaxed)
                ));
            }
            let count = series.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "nexo_tool_latency_ms_bucket{{agent=\"{agent}\",tool=\"{tool}\",le=\"+Inf\"}} {count}\n"
            ));
            out.push_str(&format!(
                "nexo_tool_latency_ms_sum{{agent=\"{agent}\",tool=\"{tool}\"}} {}\n",
                series.sum_ms.load(Ordering::Relaxed)
            ));
            out.push_str(&format!(
                "nexo_tool_latency_ms_count{{agent=\"{agent}\",tool=\"{tool}\"}} {count}\n"
            ));
        }
    }

    out.push_str(
        "# HELP nexo_link_understanding_fetch_total Link-understanding fetch outcomes by result.\n",
    );
    out.push_str("# TYPE nexo_link_understanding_fetch_total counter\n");
    if LINK_FETCH_TOTAL.is_empty() {
        out.push_str("nexo_link_understanding_fetch_total{result=\"\"} 0\n");
    } else {
        let mut rows: Vec<(String, u64)> = LINK_FETCH_TOTAL
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (result, v) in rows {
            out.push_str(&format!(
                "nexo_link_understanding_fetch_total{{result=\"{}\"}} {}\n",
                escape(&result),
                v
            ));
        }
    }

    out.push_str(
        "# HELP nexo_link_understanding_cache_total Link-understanding cache lookups by hit.\n",
    );
    out.push_str("# TYPE nexo_link_understanding_cache_total counter\n");
    if LINK_CACHE_TOTAL.is_empty() {
        out.push_str("nexo_link_understanding_cache_total{hit=\"\"} 0\n");
    } else {
        let mut rows: Vec<(String, u64)> = LINK_CACHE_TOTAL
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (hit, v) in rows {
            out.push_str(&format!(
                "nexo_link_understanding_cache_total{{hit=\"{}\"}} {}\n",
                escape(&hit),
                v
            ));
        }
    }

    out.push_str("# HELP nexo_link_understanding_fetch_duration_ms Per-fetch latency histogram.\n");
    out.push_str("# TYPE nexo_link_understanding_fetch_duration_ms histogram\n");
    {
        let h = &*LINK_FETCH_DURATION;
        for (idx, upper) in LATENCY_BUCKET_LIMITS_MS.iter().enumerate() {
            out.push_str(&format!(
                "nexo_link_understanding_fetch_duration_ms_bucket{{le=\"{upper}\"}} {}\n",
                h.buckets[idx].load(Ordering::Relaxed)
            ));
        }
        let count = h.count.load(Ordering::Relaxed);
        out.push_str(&format!(
            "nexo_link_understanding_fetch_duration_ms_bucket{{le=\"+Inf\"}} {count}\n"
        ));
        out.push_str(&format!(
            "nexo_link_understanding_fetch_duration_ms_sum {}\n",
            h.sum_ms.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "nexo_link_understanding_fetch_duration_ms_count {count}\n"
        ));
    }

    out.push_str(
        "# HELP pairing_inbound_challenged_total Pairing challenge delivery attempts by channel and result.\n",
    );
    out.push_str("# TYPE pairing_inbound_challenged_total counter\n");
    if PAIRING_INBOUND_CHALLENGED.is_empty() {
        out.push_str("pairing_inbound_challenged_total{channel=\"\",result=\"\"} 0\n");
    } else {
        let mut rows: Vec<(PairingChallengeKey, u64)> = PAIRING_INBOUND_CHALLENGED
            .iter()
            .map(|e| (e.key().clone(), e.value().load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| {
            (a.0.channel.clone(), a.0.result.clone())
                .cmp(&(b.0.channel.clone(), b.0.result.clone()))
        });
        for (key, v) in rows {
            out.push_str(&format!(
                "pairing_inbound_challenged_total{{channel=\"{}\",result=\"{}\"}} {}\n",
                escape(&key.channel),
                escape(&key.result),
                v,
            ));
        }
    }

    nexo_web_search::telemetry::render(&mut out);
    nexo_pairing::telemetry::render(&mut out);

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
                "nexo_tool_calls_total{agent=\"kate\",outcome=\"ok\",tool=\"mcp_fs_read\"} 2"
            ),
            "missing ok counter:\n{body}"
        );
        assert!(contains_line(
            &body,
            "nexo_tool_calls_total{agent=\"kate\",outcome=\"error\",tool=\"mcp_fs_read\"} 1"
        ));
        assert!(contains_line(
            &body,
            "nexo_tool_latency_ms_count{agent=\"kate\",tool=\"mcp_fs_read\"} 2"
        ));
        assert!(
            body.contains(
                "nexo_tool_latency_ms_bucket{agent=\"kate\",tool=\"mcp_fs_read\",le=\"50\"}"
            ),
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
        assert!(
            body.contains("\"le=\\\"50\\\"".to_string().as_str()) || body.contains("le=\"50\"")
        );
        assert!(body.contains(
            "llm_latency_ms_sum{agent=\"kate\",provider=\"minimax\",model=\"MiniMax-Text-01\"} 42"
        ));
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
    fn extension_discovery_status_metrics_render() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        add_extensions_discovered("ok", 2);
        add_extensions_discovered("disabled", 1);
        let body = render_prometheus(false);
        assert!(body.contains("nexo_extensions_discovered{status=\"ok\"} 2"));
        assert!(body.contains("nexo_extensions_discovered{status=\"disabled\"} 1"));
        assert!(body.contains("nexo_extensions_discovered{status=\"invalid\"} 0"));
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
