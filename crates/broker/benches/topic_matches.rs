//! `topic_matches` is the hottest path in `LocalBroker::publish` —
//! every published event runs this against every active subscription
//! pattern. With N subscriptions and M events/sec, the runtime burns
//! N×M of these per second. Sub-100ns per match is the target.
//!
//! Run with:
//!     cargo bench -p nexo-broker --bench topic_matches

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexo_broker::topic::topic_matches;

fn bench_exact_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("topic_matches/exact");
    group.throughput(Throughput::Elements(1));
    let cases = [
        ("agent.kate.inbox", "agent.kate.inbox"),                // hit
        ("agent.kate.inbox", "agent.bob.inbox"),                  // miss
        ("plugin.outbound.whatsapp", "plugin.outbound.telegram"), // miss prefix
        ("a.b.c.d.e.f.g.h", "a.b.c.d.e.f.g.h"),                  // long hit
    ];
    for (i, (pattern, subject)) in cases.iter().enumerate() {
        group.bench_with_input(
            BenchmarkId::new("case", i),
            &(pattern, subject),
            |b, (p, s)| {
                b.iter(|| black_box(topic_matches(p, s)));
            },
        );
    }
    group.finish();
}

fn bench_wildcard_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("topic_matches/wildcard");
    group.throughput(Throughput::Elements(1));
    let cases = [
        ("agent.*.inbox", "agent.kate.inbox"),       // single-wildcard hit
        ("agent.*.inbox", "agent.kate.outbox"),      // single-wildcard miss
        ("agent.>", "agent.kate.inbox.priority"),    // multi-wildcard hit
        ("plugin.outbound.>", "plugin.inbound.x"),   // multi-wildcard miss
        ("a.*.c.*.e", "a.b.c.d.e"),                  // double wildcard hit
        ("a.*.c.*.e.f", "a.b.c.d.e"),                // double wildcard miss (length)
    ];
    for (i, (pattern, subject)) in cases.iter().enumerate() {
        group.bench_with_input(
            BenchmarkId::new("case", i),
            &(pattern, subject),
            |b, (p, s)| {
                b.iter(|| black_box(topic_matches(p, s)));
            },
        );
    }
    group.finish();
}

fn bench_wildcard_storm(c: &mut Criterion) {
    // Realistic publish: a single event evaluated against 50 active
    // subscription patterns. Approximates a live deployment where each
    // agent has ~3 patterns and the broker hosts ~15 agents.
    let patterns: Vec<String> = (0..50)
        .map(|i| match i % 5 {
            0 => format!("agent.id-{i}.inbox"),
            1 => format!("agent.id-{i}.>"),
            2 => format!("plugin.outbound.*"),
            3 => format!("session.*.id-{i}"),
            _ => format!("a.b.c.id-{i}.d"),
        })
        .collect();
    let subject = "agent.id-25.inbox";

    let mut group = c.benchmark_group("topic_matches/storm");
    group.throughput(Throughput::Elements(patterns.len() as u64));
    group.bench_function("50_patterns_one_subject", |b| {
        b.iter(|| {
            let hits = patterns
                .iter()
                .filter(|p| topic_matches(p, subject))
                .count();
            black_box(hits)
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_exact_match,
    bench_wildcard_match,
    bench_wildcard_storm
);
criterion_main!(benches);
