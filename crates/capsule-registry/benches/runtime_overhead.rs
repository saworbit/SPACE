use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

// Simulates the previous behavior that created a runtime per call.
fn old_block_on_future<F: std::future::Future>(fut: F) -> F::Output {
    Runtime::new()
        .expect("failed to build tokio runtime")
        .block_on(fut)
}

// Shared runtime initialized once for all sync bridge calls.
fn global_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("Failed to create global runtime")
    })
}

fn new_block_on_future<F: std::future::Future>(fut: F) -> F::Output {
    global_runtime().block_on(fut)
}

// Dummy future that does trivial work.
async fn dummy_work() -> u64 {
    42
}

fn bench_runtimes(c: &mut Criterion) {
    let mut group = c.benchmark_group("Runtime Overhead");

    group.bench_function("Old: Create Runtime Per Call", |b| {
        b.iter(|| {
            let res = old_block_on_future(dummy_work());
            black_box(res);
        })
    });

    group.bench_function("New: Global Static Runtime", |b| {
        b.iter(|| {
            let res = new_block_on_future(dummy_work());
            black_box(res);
        })
    });

    group.finish();
}

criterion_group!(benches, bench_runtimes);
criterion_main!(benches);
