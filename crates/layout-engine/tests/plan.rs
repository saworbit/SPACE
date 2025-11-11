use common::{LayoutStrategy, Policy};
use layout_engine::LayoutEngine;

#[test]
fn fixed_fallback_compatible() {
    let mut policy = Policy::default();
    policy.layout.strategy = LayoutStrategy::Fixed {
        segment_size: 4 * 1024 * 1024,
    };

    let engine = LayoutEngine::new(&policy);
    let data = vec![0u8; 8 * 1024 * 1024];
    let plan = engine
        .synthesize(&[], &[&data[..]], &policy)
        .expect("fixed plan should succeed");

    assert_eq!(plan.zones.len(), 2);
}
