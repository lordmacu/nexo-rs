//! `WaitEngine::tick` benchmark — the most-called path in the
//! TaskFlow runtime: every interval the engine scans every flow with
//! `WaitCondition::Timer` to see if any expired. Sub-millisecond is
//! the target at single-host scale; this bench measures the path at
//! 10 / 100 / 1 000 active waiting flows so a regression on the SQL
//! query or the in-memory cursor logic shows up immediately.
//!
//! Run with:
//!     cargo bench -p nexo-taskflow --bench tick

use std::sync::Arc;

use chrono::{Duration, Utc};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nexo_taskflow::engine::{WaitCondition, WaitEngine};
use nexo_taskflow::manager::{CreateManagedInput, FlowManager};
use nexo_taskflow::store::SqliteFlowStore;
use serde_json::json;

/// Build an in-memory SQLite-backed `FlowManager` + `WaitEngine` pair.
/// Fast (sub-100ms per setup) and hermetic; no on-disk state leaks
/// between iterations.
async fn make_engine(num_waiting: usize) -> WaitEngine {
    let store = SqliteFlowStore::open(":memory:")
        .await
        .expect("in-memory sqlite open");
    let manager = FlowManager::new(Arc::new(store));

    // Seed N waiting flows with future-timer waits. The tick path
    // scans the store; a future timer means none are due yet, so we
    // measure the scan cost without I/O for resume.
    let future = Utc::now() + Duration::seconds(3600);
    for i in 0..num_waiting {
        let flow = manager
            .create_managed(CreateManagedInput {
                controller_id: format!("bench-controller-{i}"),
                goal: format!("bench goal {i}"),
                owner_session_key: format!("session-{i}"),
                requester_origin: "bench".into(),
                current_step: "step-0".into(),
                state_json: json!({}),
            })
            .await
            .expect("create_managed");
        manager.start_running(flow.id).await.expect("start_running");
        let wait = WaitCondition::Timer { at: future };
        manager
            .set_waiting(flow.id, wait.into_value())
            .await
            .expect("set_waiting");
    }

    WaitEngine::new(manager)
}

fn bench_tick_no_due(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut group = c.benchmark_group("waitengine/tick_no_due");
    for n in [10usize, 100, 1_000].iter() {
        let engine = runtime.block_on(make_engine(*n));
        group.throughput(Throughput::Elements(*n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &_n| {
            b.iter(|| {
                runtime.block_on(async {
                    let report = engine.tick().await;
                    std::hint::black_box(report);
                });
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_tick_no_due);
criterion_main!(benches);
