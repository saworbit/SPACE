# Patentable Concepts in **SPACE**

> **Purpose** – Capture the technically distinctive mechanisms that *may* warrant patent protection.  Each section summarises novelty, presents design detail (pseudocode + diagrams) and positions the idea against known prior art.  This document itself is **not** a patent application; it serves as documented evidence of conception and prior art.

---

## 1  Universal Object Namespace with Protocol Views

### 1.1  Novelty claim

*Single 128‑bit object ID mapped at runtime to block, file or object semantics, governed by policy labels.* Existing “unified” systems still store data in separate silos internally; SPACE stores once, projects many views.

### 1.2  Technical detail

```
object_id -> [segment list] -> [protocol adapter] -> client
```

Pseudocode (Rust‑like):

```rust
fn open_view(id: Uuid, proto: Protocol) -> View {
    let meta = metadata::lookup(id)?;
    match proto {
        Protocol::Block => View::Block(BlockView::new(meta)),
        Protocol::File  => View::File(FileView::new(meta)),
        Protocol::Object => View::Object(ObjectView::new(meta)),
    }
}
```

### 1.3  Sequence diagram

```
Client ─► API ─► metadata.lookup(id) ─► attach view attrs ─► return handle
```

### 1.4  Prior art & differentiation

NetApp FlexVol, Pure Volumes, VAST Containers – store block/file separately; require copy or refcount tricks.  SPACE uses one object map.

---

## 2  Composable Offload Framework

### 2.1  Novelty claim

*Same data‑service crate runs on CPU, DPU, GPU or computational SSD without feature drift; offload selected dynamically at plug‑in time.*

### 2.2  Technical detail

```rust
trait Offload {
    fn checksum(&self, buf: &[u8]) -> u32;
    fn compress(&self, seg: &[u8]) -> Vec<u8>;
    fn erasure_encode(&self, stripe: &[Segment]) -> Vec<Parity>;
}
static REGISTRY: RwLock<HashMap<HardwareId, Box<dyn Offload>>> = ...;
```

ASCII block:

```
+-------+  register  +-------------+
|  DPU  |───────────►|  REGISTRY   |
+-------+            +------+------+
                           │ select()
                           ▼
                     +-----+------+
                     | IO Engine  |
                     +------------+
```

### 2.3  Differentiation

NVIDIA BlueField SDK provides fixed functions; SPACE allows *any* accelerator adhering to the trait—first of its kind.

---

## 3  Per‑Segment Encryption with Inline Dedup & Compression

### 3.1  Novelty claim

Encrypt XTS‑AES‑256 per 256 MiB segment *after* compression + dedupe yet retain global dedupe across ciphertext via deterministic IV derivation.

```
IV = SHA256(volume_uuid || segment_offset)
key_seg = KEK_derive(volume_uuid, seg_no)
cipher = AES_XTS(key_seg, IV, plaintext)
```

This yields identical ciphertext for identical plaintext globally, enabling dedupe over encrypted data.

### 3.2  Pseudocode

```rust
fn encrypt_segment(seg: &Plain, vol: Uuid, off: u64) -> Cipher {
    let iv  = sha256!(vol, off);
    let key = kdf::derive(&vol, off);
    aes_xts::encrypt(&key, iv, seg)
}
```

### 3.3  Prior art

Most vendors disable dedupe when at‑rest encryption is on (e.g., Pure SafeMode).  SPACE maintains both.

---

## 4  Confidential Compute Job Slots Inside the Array

### 4.1  Novelty claim

SGX/SEV enclave spawned *within* storage node, running WASM/Python directly on encrypted segments – no data egress.

### 4.2  Flow diagram

```
[User Query]
     │ gRPC
     ▼
 Policy Compiler ─► allocate enclave ─► load code + keys ─► run │
                                                            ▼
                                                      Results (sealed)
```

### 4.3  Pseudocode stub

```rust
fn run_enclave(code: Wasm, segs: &[Uuid]) -> SealedResult {
    let enclave = sgx::spawn()?;
    enclave.load_code(code)?;
    enclave.map_segments(segs)?;
    enclave.exec()?;
    enclave.seal()
}
```

---

## 5  On‑Write Merkle Chain Anchored to Rotating KEK

### 5.1  Claim

Each snapshot root stores `MerkleRoot = H(root_id || KEK_version)`.  Rotating KEK creates tamper‑evident hash lineage across snapshots; proofs exportable.

### 5.2  Diagram

```
Snap0 ─► Snap1 ─► Snap2
 |          |          |
 H0         H1         H2
```

`Hi = H( object_hashes_i || KEK_i )`

---

## 6  eBPF Micro‑Segmentation Gateway for Storage Paths

### 6.1  Claim

First packet ingest in kernel eBPF validates SPIFFE cert, enforces QoS + tenant tags before user‑space handoff.

### 6.2  Hook points

```
TC ingress: parse TLS client‑hello
             ├─ verify cert against mesh CA
             └─ apply cgroup id = tenant
```

---

## 7  Policy Compiler Optimisation Engine

### 7.1  Claim

Constraint‑solver chooses cheapest workflow path that satisfies RPO/RTO, latency and hardware availability, hot‑swappable at runtime.

Solver input (example):

```
Policy:
  RPO <= 900s
  Metro latency <= 2ms
  Budget <= 20% CPU spare
```

Output: “Async fan‑out + nightly snap” or “Metro‑Sync + hourly snap”

---

## Contributors & Contact

*Shane Wall* (concept synthesis)\
*ChatGPT (OpenAI o3)* (drafting assistance)

For legal assessment email **ip\@adaptive‑storage.dev**.

---

© 2025  Shane Wall & contributors.  Licensed under Apache 2.0 (no patent licence implied).

