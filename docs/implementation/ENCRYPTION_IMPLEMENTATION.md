# Encryption Implementation

This document covers how SPACE encrypts segments on the post-reset `main` branch: XTS-AES-256 with a deterministic tweak, a BLAKE3 keyed MAC, and HKDF-derived keys.

**Status:**
- The encryption path works through the library API.
- `spacectl create` and S3 PUT always write with the default policy, which has encryption disabled, so they never encrypt ([#82](https://github.com/saworbit/SPACE/issues/82)).
- The crate hasn't had an external review.

## Where the code lives

| File | Contents |
|------|----------|
| `crates/encryption/src/xts.rs` | `encrypt`, `decrypt`, `encrypt_segment`, `decrypt_segment`, `derive_tweak_from_hash` |
| `crates/encryption/src/mac.rs` | `compute_mac`, `verify_mac` (BLAKE3 keyed MAC) |
| `crates/encryption/src/keymanager.rs` | `KeyManager`, `XtsKeyPair`, HKDF key derivation |
| `crates/encryption/src/key_provider.rs` | `KeyProvider` trait: `EnvKeyProvider`, `FileKeyProvider`, and `AwsKmsProvider` / `HashiCorpVaultProvider` stubs that return "not implemented" |
| `crates/encryption/src/policy.rs` | `EncryptionMetadata` |
| `crates/encryption/src/error.rs` | `EncryptionError` |
| `crates/pipeline/src/lib.rs` | `XtsEncryptor` and `NoopEncryptor`, plus where the write and read paths call them |

The pipeline uses `common::EncryptionPolicy`:

```rust
pub enum EncryptionPolicy {
    Disabled,                                 // default
    XtsAes256 { key_version: Option<u32> },  // None = current key version
}
```

`Policy::encrypted()` (LZ4) and `Policy::encrypted_compressed()` (Zstd level 3) are the presets that turn it on. The `encryption` crate also defines its own `EncryptionPolicy` (`None` / `XtsAes256 { key_version: u32 }`), which the pipeline doesn't use.

## Keys

### Master key

The master key is 32 bytes. Ways to load it:

- `KeyManager::from_env()` reads `SPACE_MASTER_KEY`, which must be exactly 64 hex characters.
- `KeyManager::new([u8; 32])`.
- `KeyManager::from_provider(&dyn KeyProvider).await`.
- `KeyManager::from_tpm(&impl TpmProvider)`.
- `KeyManager::generate()`, which creates a random key.

### Derivation

Derivation is HKDF-HMAC-SHA256, hand-rolled on the `hmac` and `sha2` crates:

```
salt   = BLAKE3("SPACE-HKDF-SALT-V1" || master_key)[..32]   (or a salt supplied by a TPM provider)
prk    = HMAC-SHA256(salt, master_key)
okm    = HKDF-Expand(prk, info = "SPACE-XTS-AES-256-KEY-V1" || version as u32 big-endian, 64 bytes)
key1   = okm[..32]   (data key)
key2   = okm[32..]   (tweak key)
```

`get_key(version)` derives a version on first use and caches it. Every version can be re-derived from the master key, so anyone holding the master key can read every version.

### Rotation

`rotate()` increments the current version and errors while a previous rotation hasn't been marked done with `complete_rotation()`. Segments record the `key_version` they were written with and stay readable after a rotation.

Rotation is in memory only: every constructor starts at version 1, so a restart undoes it, and nothing exposes it through `spacectl` ([#98](https://github.com/saworbit/SPACE/issues/98)).

### Zeroization

- `XtsKeyPair` derives `Zeroize` and `ZeroizeOnDrop`.
- `KeyManager`'s `Drop` zeroizes the master key and the salt.
- `XtsEncryptor` clones the key pair for each segment; the clones zeroize when dropped.

### Fallbacks to know about

These are tracked in [#84](https://github.com/saworbit/SPACE/issues/84):

- If `SPACE_MASTER_KEY` is missing or invalid, `WritePipeline::new` and the registry pipeline builders quietly choose the plaintext pipeline (`NoopEncryptor`).
- `CapsuleRegistry::open` falls back to an all-zero master key.
- `EnvKeyProvider` falls back to a fixed key when its variable is unset.

## XTS-AES-256

- Built on the `xts-mode` and `aes` crates. `aes` uses AES-NI when the CPU supports it.
- Each segment is encrypted as a single XTS data unit with one tweak. The ciphertext is the same length as the input.
- Input must be at least 16 bytes, or encryption fails with `InvalidCiphertextLength`. Encrypted writes of 1–15 byte objects, or of objects whose last segment is under 16 bytes, therefore fail ([#94](https://github.com/saworbit/SPACE/issues/94)).
- **Tweak:** `derive_tweak_from_hash(blake3(compressed_segment))`, the first 16 bytes of a plain BLAKE3 hash of the bytes being encrypted. This is not the dedup `content_hash`, which also mixes in the compression algorithm. The tweak is stored in the segment's `tweak_nonce`.
- **Deterministic:** the same compressed bytes under the same key version always produce the same ciphertext. That is what lets encrypted segments be shared between capsules. It also means anyone who can read the disk can see which segments are equal.

## MAC

```
mac_key = BLAKE3("SPACE-BLAKE3-MAC-KEY-V1" || key1 || key2)
tag     = BLAKE3_keyed(mac_key, ciphertext || encryption_version (u16 LE) || key_version (u32 LE)
                                || tweak (16 B) || ciphertext_len (u32 LE))[..16]
```

- Each metadata field is included only when it's set.
- `verify_mac` compares tags with `subtle::ConstantTimeEq` and runs before decryption.
- The MAC does **not** cover the segment ID, the capsule ID, the `encrypted` / `compressed` flags, or `wrapped_segment_key`.
- Whether a segment gets verified at all depends on the `encrypted` flag in the segment sidecar. Clearing that flag skips verification, and unencrypted segments aren't integrity-checked on read ([#95](https://github.com/saworbit/SPACE/issues/95)).

## Write path

`Pipeline::write_capsule` handles each chunk of the input (4 MiB by default) as follows:

1. **Compress.** The chunk is stored as `identity` (uncompressed) if the first 1 KiB has entropy of 7.5 bits/byte or more, or if compression doesn't make it smaller.
2. **Hash.** `content_hash = hash_content_with_algo(compressed, algo)`.
3. **Dedup lookup.**
   - **Hit:** increment the existing segment's `ref_count` and reuse it. Nothing is encrypted or written.
   - The lookup ignores the encryption policy, so an encrypted write can reuse a plaintext segment ([#83](https://github.com/saworbit/SPACE/issues/83)).
4. **Miss:** if the policy enables encryption, `XtsEncryptor::encrypt` produces the ciphertext, tweak, key version and MAC tag. The bytes are appended to the log, the segment metadata is written with those fields, and `content_hash` is registered.
   - `encrypted` is set from the *policy*. With `NoopEncryptor` (no key loaded), the stored bytes are plaintext but still marked encrypted ([#84](https://github.com/saworbit/SPACE/issues/84)).
5. **Record the capsule.** Once every chunk is stored, the capsule record is created with its list of segment IDs.

## Read path

For each segment of the capsule:

1. Read the stored bytes.
2. If `segment.encrypted` is set:
   - build the MAC metadata from the segment record (`tweak_nonce`, `key_version`, `encryption_version`, length);
   - load that key version;
   - verify the MAC;
   - decrypt.
3. If `segment.compressed` is set, decompress according to `compression_algo`.

The read path uses the segment record's flags, never `capsule.policy`. If a segment has no `tweak_nonce`, the reader falls back to deriving a tweak from `content_hash`. That gives a different value from the one the writer used, so verification fails; segments written by the pipeline always record a tweak.

## Using it

```bash
export SPACE_MASTER_KEY=$(openssl rand -hex 32)   # 64 hex characters
```

```rust
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::Policy;
use nvram_sim::NvramLog;

let pipeline = WritePipeline::new(CapsuleRegistry::open("space.db")?, NvramLog::open("space.nvram")?);
let id = pipeline.write_capsule_with_policy(&data, &Policy::encrypted()).await?;
let plaintext = pipeline.read_capsule(id).await?;
```

To pass a key explicitly instead of reading the environment, use `WritePipeline::with_key_manager(registry, nvram, key_manager)`.

## Tests

```bash
cargo test -p encryption                                    # unit tests for XTS, MAC, key manager, providers
cargo test -p pipeline                                      # XtsEncryptor / NoopEncryptor
cargo test -p capsule-registry --test proptest_pipeline     # includes prop_roundtrip_encrypted
```

## Performance

These numbers are measured end to end through `Pipeline` on 512 MiB of random data, so compression was skipped. Setup: release build, `NvramBackend`, Ryzen 9 5900X, NVMe, Windows 11.

| | Write | Read |
|---|---|---|
| Plaintext | 273 MB/s | 954 MB/s |
| Encrypted (XTS + MAC) | 205 MB/s | 334 MB/s |

Writes are dominated by an fsync per 4 MiB segment and the sidecar rewrite. Reads pay for single-threaded XTS, a separate MAC pass, and buffer copies. There's no end-to-end benchmark in the repo yet ([#102](https://github.com/saworbit/SPACE/issues/102)). The closest is the per-stage microbenchmark:

```bash
cargo bench -p capsule-registry --bench pipeline -- encrypt_xts_segment
```

## Not implemented

- **Post-quantum key wrapping:** `CryptoProfile::HybridKyber` exists, and `Policy::validate` rejects it when dedupe is on. There's no ML-KEM code, though: segments written under it are encrypted exactly like `Classical`, and the `pq_ciphertext` / `pq_nonce` fields stay `None`.
- **KMS and Vault providers:** stubs that return errors.
- **Re-encryption** of existing segments after a key rotation.
