//! End-to-end publish benchmark for `LocalBroker`. Measures the path
//! every inbound event runs through: lock-free pattern scan over the
//! `DashMap<pattern, Sender>`, `try_send` per matching subscriber,
//! slow-consumer drop-counter increments.
//!
//! Run with:
//!     cargo bench -p nexo-broker --bench local_publish

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use nexo_broker::types::Event;
use nexo_broker::{BrokerHandle, LocalBroker};
use serde_json::json;

fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
}

fn make_event(topic: &str) -> Event {
    Event::new(topic, "bench", json!({"hello": "world"}))
}

fn bench_publish_no_subs(c: &mut Criterion) {
    // Worst-case "miss": no subscribers match. We still pay the
    // pattern scan and the clone-once-skipped path.
    let runtime = make_runtime();
    let broker = LocalBroker::new();
    let event = make_event("agent.kate.inbox");

    let mut group = c.benchmark_group("publish");
    group.throughput(Throughput::Elements(1));
    group.bench_function("no_subscribers", |b| {
        b.iter(|| {
            runtime.block_on(async {
                broker
                    .publish(black_box("agent.kate.inbox"), event.clone())
                    .await
                    .unwrap();
            });
        });
    });
    group.finish();
}

fn bench_publish_one_sub(c: &mut Criterion) {
    let runtime = make_runtime();
    let broker = LocalBroker::new();
    let _sub = runtime.block_on(async {
        broker.subscribe("agent.kate.inbox").await.unwrap()
    });
    let event = make_event("agent.kate.inbox");

    let mut group = c.benchmark_group("publish");
    group.throughput(Throughput::Elements(1));
    group.bench_function("one_subscriber_exact", |b| {
        b.iter(|| {
            runtime.block_on(async {
                broker
                    .publish(black_box("agent.kate.inbox"), event.clone())
                    .await
                    .unwrap();
            });
        });
    });
    group.finish();
}

fn bench_publish_fanout_10(c: &mut Criterion) {
    let runtime = make_runtime();
    let broker = LocalBroker::new();
    let _subs: Vec<_> = (0..10)
        .map(|_| {
            runtime.block_on(async {
                broker.subscribe("agent.>").await.unwrap()
            })
        })
        .collect();
    let event = make_event("agent.kate.inbox");

    let mut group = c.benchmark_group("publish");
    group.throughput(Throughput::Elements(10));
    group.bench_function("fanout_10_wildcard", |b| {
        b.iter(|| {
            runtime.block_on(async {
                broker
                    .publish(black_box("agent.kate.inbox"), event.clone())
                    .await
                    .unwrap();
            });
        });
    });
    group.finish();
}

fn bench_publish_mixed_50(c: &mut Criterion) {
    // Realistic shape: 50 active subscriptions across 5 patterns, one
    // publish hits the wildcard chunk only. Approximates a 15-agent
    // deployment with ~3 patterns each.
    let runtime = make_runtime();
    let broker = LocalBroker::new();
    let patterns = [
        "agent.kate.>",
        "agent.bob.>",
        "plugin.outbound.*",
        "session.*",
        "agent.>",
    ];
    let _subs: Vec<_> = (0..50)
        .map(|i| {
            let pattern = patterns[i % patterns.len()];
            runtime.block_on(async {
                broker.subscribe(pattern).await.unwrap()
            })
        })
        .collect();
    let event = make_event("agent.kate.inbox");

    let mut group = c.benchmark_group("publish");
    group.throughput(Throughput::Elements(50));
    group.bench_function("mixed_50_subs", |b| {
        b.iter(|| {
            runtime.block_on(async {
                broker
                    .publish(black_box("agent.kate.inbox"), event.clone())
                    .await
                    .unwrap();
            });
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_publish_no_subs,
    bench_publish_one_sub,
    bench_publish_fanout_10,
    bench_publish_mixed_50
);
criterion_main!(benches);
