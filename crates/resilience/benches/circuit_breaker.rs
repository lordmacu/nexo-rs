//! Microbenchmarks for `CircuitBreaker`. Run with:
//!
//!     cargo bench -p nexo-resilience
//!
//! Output goes to `target/criterion/`. Phase 35 scaffolding — these
//! cover the hottest paths (`allow`, `on_success`, `on_failure`) so
//! a regression on the breaker shows up immediately under
//! `cargo bench` in CI. Add new groups for closed→open transitions,
//! contention scenarios, and the async `call()` wrapper as the
//! benchmark suite grows.

use std::sync::Arc;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};

fn config() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 5,
        success_threshold: 2,
        initial_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(10),
    }
}

/// Hot path #1 — every external call queries `allow()` before
/// dispatching. Closed-state allow should be a sub-100ns atomic
/// load. If this regresses we'll feel it in every LLM call.
fn bench_allow_closed(c: &mut Criterion) {
    let breaker = CircuitBreaker::new("bench-closed", config());
    let mut group = c.benchmark_group("allow");
    group.throughput(Throughput::Elements(1));
    group.bench_function("closed", |b| {
        b.iter(|| {
            black_box(breaker.allow());
        });
    });
    group.finish();
}

/// Same call against an open breaker. Should be the same atomic load
/// + a clock check; no observable diff.
fn bench_allow_open(c: &mut Criterion) {
    let breaker = CircuitBreaker::new("bench-open", config());
    breaker.trip();
    let mut group = c.benchmark_group("allow");
    group.throughput(Throughput::Elements(1));
    group.bench_function("open", |b| {
        b.iter(|| {
            black_box(breaker.allow());
        });
    });
    group.finish();
}

/// `on_success` increments the success counter. Hot path on every
/// successful external call.
fn bench_on_success(c: &mut Criterion) {
    let breaker = CircuitBreaker::new("bench-success", config());
    let mut group = c.benchmark_group("transitions");
    group.throughput(Throughput::Elements(1));
    group.bench_function("on_success", |b| {
        b.iter(|| {
            breaker.on_success();
        });
    });
    group.finish();
}

/// `on_failure` increments the failure counter and may flip the
/// breaker open. Worst-case path measured here.
fn bench_on_failure(c: &mut Criterion) {
    let breaker = CircuitBreaker::new("bench-failure", config());
    let mut group = c.benchmark_group("transitions");
    group.throughput(Throughput::Elements(1));
    group.bench_function("on_failure", |b| {
        b.iter(|| {
            breaker.on_failure();
        });
    });
    group.finish();
}

/// Contention check — N tasks hammer the same breaker simultaneously.
/// Reveals lock contention or atomic-CAS retry pressure.
fn bench_concurrent_allow(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    let mut group = c.benchmark_group("concurrent");
    group.throughput(Throughput::Elements(8));
    group.bench_function("8x_allow", |b| {
        b.iter_with_setup(
            || Arc::new(CircuitBreaker::new("bench-concurrent", config())),
            |breaker| {
                runtime.block_on(async move {
                    let handles: Vec<_> = (0..8)
                        .map(|_| {
                            let cb = Arc::clone(&breaker);
                            tokio::spawn(async move {
                                for _ in 0..1000 {
                                    black_box(cb.allow());
                                }
                            })
                        })
                        .collect();
                    for h in handles {
                        h.await.unwrap();
                    }
                });
            },
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_allow_closed,
    bench_allow_open,
    bench_on_success,
    bench_on_failure,
    bench_concurrent_allow,
);
criterion_main!(benches);
