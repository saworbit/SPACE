#![no_main]
use common::Policy;
use layout_engine::LayoutEngine;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let policy = Policy::default();
    let engine = LayoutEngine::new(&policy);
    let _ = engine.synthesize(&[], &[data], &policy);
});
