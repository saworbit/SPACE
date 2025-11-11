use common::{LayoutStrategy, Policy};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use layout_engine::LayoutEngine;

fn bench_fixed(c: &mut Criterion) {
    let mut policy = Policy::default();
    policy.layout.strategy = LayoutStrategy::Fixed {
        segment_size: 4 * 1024 * 1024,
    };
    let data = vec![0u8; 100 * 1024 * 1024];
    c.bench_function("fixed_4mib", |b| {
        b.iter(|| {
            let engine = LayoutEngine::new(&policy);
            let _ = engine.synthesize(black_box(&[]), black_box(&[&data[..]]), &policy);
        })
    });
}

criterion_group!(layout_bench, bench_fixed);
criterion_main!(layout_bench);
