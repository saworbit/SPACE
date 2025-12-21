#![cfg(feature = "phase5")]

use common::policy::{ResourceLimits, TransformDef, TransformTrigger};
use common::Policy;
use pipeline::PipelineBuilder;
use std::collections::HashMap;

const APPEND_EXCLAMATION_WASM: &[u8] = &[
    0, 97, 115, 109, 1, 0, 0, 0, 1, 17, 3, 96, 1, 127, 1, 127, 96, 2, 127, 127, 0, 96, 2, 127, 127,
    1, 126, 3, 4, 3, 0, 1, 2, 5, 3, 1, 0, 1, 6, 7, 1, 127, 1, 65, 128, 8, 11, 7, 38, 4, 6, 109,
    101, 109, 111, 114, 121, 2, 0, 5, 97, 108, 108, 111, 99, 0, 0, 7, 100, 101, 97, 108, 108, 111,
    99, 0, 1, 7, 112, 114, 111, 99, 101, 115, 115, 0, 2, 10, 99, 3, 15, 1, 1, 127, 35, 0, 34, 1,
    32, 0, 106, 36, 0, 32, 1, 11, 2, 0, 11, 78, 1, 2, 127, 32, 1, 65, 1, 106, 16, 0, 33, 2, 65, 0,
    33, 3, 2, 64, 3, 64, 32, 3, 32, 1, 79, 13, 1, 32, 2, 32, 3, 106, 32, 0, 32, 3, 106, 45, 0, 0,
    58, 0, 0, 32, 3, 65, 1, 106, 33, 3, 12, 0, 11, 11, 32, 2, 32, 1, 106, 65, 33, 58, 0, 0, 32, 2,
    173, 66, 32, 134, 32, 1, 65, 1, 106, 173, 132, 11, 0, 76, 4, 110, 97, 109, 101, 1, 8, 1, 0, 5,
    97, 108, 108, 111, 99, 2, 33, 2, 0, 2, 0, 3, 108, 101, 110, 1, 3, 112, 116, 114, 2, 4, 0, 3,
    112, 116, 114, 1, 3, 108, 101, 110, 2, 3, 111, 117, 116, 3, 1, 105, 3, 15, 1, 2, 2, 0, 4, 100,
    111, 110, 101, 1, 4, 108, 111, 111, 112, 7, 7, 1, 0, 4, 104, 101, 97, 112,
];

fn write_wasm_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("append_exclamation.wasm");

    std::fs::write(&path, APPEND_EXCLAMATION_WASM).expect("write wasm");

    let uri = format!("file://{}", path.to_string_lossy().replace('\\', "/"));
    (dir, uri)
}

#[tokio::test]
async fn on_read_transform_applies_to_stream() {
    let (_dir, wasm_uri) = write_wasm_file();

    let policy = Policy {
        transform: vec![TransformDef {
            name: "append_exclamation".into(),
            image: wasm_uri,
            trigger: TransformTrigger::OnRead,
            args: HashMap::new(),
            resources: ResourceLimits::default(),
            verification: None,
        }],
        ..Policy::default()
    };

    let mut pipeline: pipeline::InMemoryPipeline = PipelineBuilder::new().build();
    let capsule_id = pipeline
        .write_capsule(b"hello", &policy)
        .await
        .expect("write capsule");

    let bytes = pipeline
        .read_capsule(capsule_id)
        .await
        .expect("read capsule");

    assert_eq!(bytes, b"hello!");
}

#[tokio::test]
async fn on_write_transform_persists_transformed_bytes() {
    let (_dir, wasm_uri) = write_wasm_file();

    let policy = Policy {
        transform: vec![TransformDef {
            name: "append_exclamation".into(),
            image: wasm_uri,
            trigger: TransformTrigger::OnWrite,
            args: HashMap::new(),
            resources: ResourceLimits::default(),
            verification: None,
        }],
        ..Policy::default()
    };

    let mut pipeline: pipeline::InMemoryPipeline = PipelineBuilder::new().build();
    let capsule_id = pipeline
        .write_capsule(b"hello", &policy)
        .await
        .expect("write capsule");

    let bytes = pipeline
        .read_capsule(capsule_id)
        .await
        .expect("read capsule");

    assert_eq!(bytes, b"hello!");
}
