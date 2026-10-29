//! End-to-end write and read throughput through the registry pipeline, on
//! an `NvramBackend` in a temporary directory. The payload is pseudo-random,
//! so compression is skipped and every segment is unique.
//!
//! ```bash
//! cargo bench -p capsule-registry --bench end_to_end
//! ```

use std::path::PathBuf;

use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::{EncryptionPolicy, Policy};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;

const PAYLOAD_SIZE: usize = 64 * 1024 * 1024;

fn payload() -> Vec<u8> {
    // xorshift64: incompressible, deterministic, no extra dependency.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut data = Vec::with_capacity(PAYLOAD_SIZE);
    while data.len() < PAYLOAD_SIZE {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(&state.to_le_bytes());
    }
    data.truncate(PAYLOAD_SIZE);
    data
}

fn policy(encrypted: bool) -> Policy {
    let mut policy = Policy::default();
    if encrypted {
        policy.encryption = EncryptionPolicy::XtsAes256 { key_version: None };
    }
    policy
}

/// A fresh store that is deleted on drop.
struct Store {
    dir: PathBuf,
    pipeline: Option<WritePipeline>,
}

impl Store {
    fn open(encrypted: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("space_bench_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
        let nvram = NvramLog::open(dir.join("space.nvram")).unwrap();
        let pipeline = if encrypted {
            let key_manager = KeyManager::new([0x42; MASTER_KEY_SIZE]);
            WritePipeline::with_key_manager(registry, nvram, key_manager)
        } else {
            WritePipeline::open(registry, nvram).unwrap()
        };
        Self {
            dir,
            pipeline: Some(pipeline),
        }
    }

    fn pipeline(&self) -> &WritePipeline {
        self.pipeline.as_ref().unwrap()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Close the log and sled before removing their files.
        self.pipeline.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn bench_end_to_end(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = payload();

    let mut group = c.benchmark_group("end_to_end");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(PAYLOAD_SIZE as u64));

    for (name, encrypted) in [("plain", false), ("encrypted", true)] {
        let policy = policy(encrypted);

        // Each write goes to an empty store, so nothing dedups.
        group.bench_function(format!("write_{name}"), |b| {
            b.iter_batched(
                || Store::open(encrypted),
                |store| {
                    rt.block_on(store.pipeline().write_capsule_with_policy(&data, &policy))
                        .unwrap();
                    store
                },
                BatchSize::PerIteration,
            )
        });

        let store = Store::open(encrypted);
        let id = rt
            .block_on(store.pipeline().write_capsule_with_policy(&data, &policy))
            .unwrap();
        group.bench_function(format!("read_{name}"), |b| {
            b.iter(|| rt.block_on(store.pipeline().read_capsule(id)).unwrap())
        });
    }

    group.finish();
}

criterion_group!(benches, bench_end_to_end);
criterion_main!(benches);
