use blake3::Hasher;
use common::{CompressionPolicy, SEGMENT_SIZE};
use compression::compress_segment;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dedup::hash_content;
use encryption::{
    derive_tweak_from_hash, encrypt_segment,
    keymanager::{KeyManager, MASTER_KEY_SIZE},
};

fn sample_payload() -> Vec<u8> {
    let mut data = Vec::with_capacity(SEGMENT_SIZE);
    for i in 0..SEGMENT_SIZE {
        let byte = ((i as u8).wrapping_mul(31)) ^ 0xA5;
        data.push(byte);
    }
    data
}

fn bench_compression(c: &mut Criterion) {
    let payload = sample_payload();
    let policy = CompressionPolicy::Zstd { level: 6 };

    c.bench_function("pipeline/compress_zstd_segment", |b| {
        b.iter(|| {
            let (_view, stats) = compress_segment(&payload, &policy).expect("compression ok");
            black_box(stats.compressed_size)
        })
    });
}

fn bench_encrypt_xts(c: &mut Criterion) {
    let payload = sample_payload();
    let mut key_material = [0u8; MASTER_KEY_SIZE];
    key_material.fill(0x42);
    let mut manager = KeyManager::new(key_material);
    let key_pair = manager.get_key(1).expect("key derived").clone();

    let mut hasher = Hasher::new();
    hasher.update(&payload);
    let tweak = derive_tweak_from_hash(hasher.finalize().as_bytes());

    c.bench_function("pipeline/encrypt_xts_segment", |b| {
        b.iter(|| {
            let (ciphertext, meta) =
                encrypt_segment(&payload, &key_pair, 1, tweak).expect("encryption ok");
            black_box(ciphertext.len() + meta.ciphertext_len.unwrap_or_default() as usize)
        })
    });
}

fn bench_dedup_hash(c: &mut Criterion) {
    let payload = sample_payload();

    c.bench_function("pipeline/dedup_hash_blake3", |b| {
        b.iter(|| {
            let hash = hash_content(&payload);
            black_box(hash);
        })
    });
}

criterion_group!(
    pipeline_benches,
    bench_compression,
    bench_encrypt_xts,
    bench_dedup_hash
);
criterion_main!(pipeline_benches);
