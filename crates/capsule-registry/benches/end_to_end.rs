//! End-to-end write and read throughput through the registry pipeline, on
//! an `NvramBackend` in a temporary directory. The payload is pseudo-random,
//! so compression is skipped and every segment is unique.
//!
//! `write_concurrent_*` writes the same 64 MiB as four 16 MiB capsules at
//! once through one pipeline, as concurrent S3 PUTs do (#198).
//!
//! ```bash
//! cargo bench -p capsule-registry --bench end_to_end
//! ```

use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::{EncryptionPolicy, Policy};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use test_support::TempDir;

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
    pipeline: WritePipeline,
    /// Last, so the log and the index are closed before their files go.
    _dir: TempDir,
}

impl Store {
    fn open(encrypted: bool) -> Self {
        let dir = TempDir::new("space_bench");
        let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
        let nvram = NvramLog::open(dir.join("space.nvram")).unwrap();
        let pipeline = if encrypted {
            let key_manager = KeyManager::new([0x42; MASTER_KEY_SIZE]);
            WritePipeline::with_key_manager(registry, nvram, key_manager)
        } else {
            WritePipeline::new(registry, nvram)
        };
        Self {
            pipeline,
            _dir: dir,
        }
    }

    fn pipeline(&self) -> &WritePipeline {
        &self.pipeline
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

        // Four writers at once, each a quarter of the payload.
        group.bench_function(format!("write_concurrent_{name}"), |b| {
            b.iter_batched(
                || Store::open(encrypted),
                |store| {
                    let writes = data
                        .chunks(PAYLOAD_SIZE / 4)
                        .map(|quarter| store.pipeline().write_reader(quarter, &policy));
                    for written in rt.block_on(futures::future::join_all(writes)) {
                        written.unwrap();
                    }
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
