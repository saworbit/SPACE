# **SPACE – Storage Platform for Adaptive Computational Ecosystems**

*A pluggable, policy‑defined data fabric built for containers, accelerators and confidential compute.*

| Status | Licence               |
| ------ | --------------------- |
|        | Apache 2.0 (intended) |

---

## 1  Why SPACE exists – problem statement

Modern workloads are scattered across VMs, containers, GPUs, DPUs and edge devices.  Traditional storage stacks split the world into block **or** file **or** object and bolt on security, scale and protection later.  SPACE starts over: **everything is an object**, every function is a container, and performance, space‑efficiency, security, mobility and self‑healing are built‑in, not bolted‑on.

*Pain points we remove*

- **Protocol silos** – no separate LUN / export / bucket worlds.
- **Forklift upgrades** – micro‑services upgraded independently.
- **Either/or trade‑offs** – encryption, snapshots and dedupe coexist without cost.
- **Human‑led operations** – health agents correct faults before tickets open.

---

## 2  Design goals

1. **Universal namespace** – one 128‑bit object ID addressable via NVMe‑oF, S3, NFS v4.2, SMB 3.2 or CSI.
2. **Stateless IO engines** – Rust + SPDK user‑space, migratable in seconds.
3. **Metadata mesh** – strongly‑consistent, FoundationDB‑style KV shards.
4. **Policy compiler** – declarative intent → executable workflows.
5. **Zero‑trust security** – SPIFFE identities, mutual TLS, TPM attestation, per‑segment keys.
6. **Composable hardware** – CPU, DPU, GPU or computational SSD selected at runtime.
7. **Autonomous repair** – health agents isolate, rebuild, re‑balance without admin.
8. **Edge‑ready** – single‑node build with eventual consistency toggle.

---

## 3  High‑level architecture

```
                   +----------------------- CONTROL PLANE -----------------------+
                   | Operators | Policy | Service | Telemetry | CLI / GraphQL  |
                   |  (CRDs)   |Compiler|  Mesh   |  Hub      | gRPC / REST    |
                   +----+------+---+----+----+----+-----+-----+------+----------+
                        |          |         |          |           |
+-----------------------v----------v---------v----------v-----------v-----------+
|                           SERVICE   MESH  (mTLS)                            |
+------------+-----------+-----------+------------+-----------+---------------+
             |           |                        |           |
             v           v                        v           v
   +---------+--+  +-----+-----+          +-------+----+  +---+----+
   |  CSI       |  |  NFS/SMB |          |   S3      |  | NVMe‑oF |
   +------+-----+  +-----+----+          +-----+-----+  +---+----+
          |               |                     |            |
          +---------------+---------------------+------------+
                              |
                              v
+-----------------------------+----------------------------------------------+
|                  eBPF POLICY GATEWAY  (SPIFFE)                             |
+-----+--------------+----------------------------------------+-------------+
      |              |                                        |
      v              v                                        v
+-----+----+  +------+-----+                          +-------+-----+
| Block   |  |  File      |                          |  Object     |
| Engine  |  |  Engine    |                          |  Engine     |
+--+------+  +------+-----+                          +------+------+
   |                 |                                      |
   |  Rust + SPDK    |                                      |
   +-----------------+-----------------+--------------------+
                                |
                                v
+-------------------------------+----------------------------------------------+
|         METADATA  MESH  (FoundationDB‑style KV, Paxos)                        |
+-------------------------------+----------------------------------------------+
           |           |                      |                     |
           v           v                      v                     v
     +-----+---+ +-----+---+           +------+----+         +------+-----+
     | Flash   | |  Disk   |           | CXL Memory|         |  NVRAM Log |
     +---------+ +---------+           +-----------+         +------------+
```

---

### 3.1 Protocol containers (current)

| Protocol facade | Crate | Notes |
|-----------------|-------|-------|
| Object (S3)     | `protocol-s3` | REST demo with in-memory key map |
| File (NFS-style)| `protocol-nfs` | Persists namespace in `space.nfs.json`, rewrites capsules on overwrite |
| Block           | `protocol-block` | Presents logical LUNs with copy-on-write updates stored in `space.block.json` |

Each facade delegates I/O to the shared `WritePipeline`, ensuring compression, dedupe, encryption, and reference management remain consistent regardless of protocol.  The lightweight adapters allow CLI tooling (`spacectl`) to expose object, file, and block semantics without duplicating storage logic.

## 4  Data flows

### 4.1 Write path – log‑structured with mirrored NVRAM

```
[App]
  │  (mTLS)
  ▼
[Protocol Container]
  │ lookup policy (labels)
  ▼
[eBPF Gateway]
  │ enforce tenant / QoS
  ▼
[IO Engine]
  │ compress + dedupe + encrypt
  ├─► mirror to peer NVRAM (RDMA, <50 µs)
  └─► append to local NVRAM        (<50 µs)
      │ background flush trigger
      ▼
[Flusher] —► Erasure‑coded Flash/Disk Tier
```

### 4.2 Read path – tier‑aware

```
[App] ─► [Protocol Container] ─► [eBPF Gateway] ─► [IO Engine]
                                             │
                                             ├─ cache hit (NVRAM)
                                             ├─ flash hit (direct)
                                             └─ disk / cold tier → promote
```

---

## 5  Reference pseudocode

### 5.1 Universal write pipeline

```rust
fn write_object(id: Uuid, data: &[u8], pol: &Policy) -> Result<()> {
    let segments = segmenter::split(data, 4 * MIB);

    let stream = segments.into_iter().map(|seg| {
        // Adaptive compression now returns Cow<[u8]> to avoid copies
        let compressed = compressor::adaptive(seg);
        let compressed_ref = compressed.as_ref();
        if dedupe::is_duplicate(compressed_ref)? { return Ok(None) } // already stored
        let ciphertext = crypto::encrypt_xts(compressed_ref, keyring::derive(&id));
        Ok(Some(ciphertext.into_owned()))
    });

    // mirrored append – returns once ACK from peer arrived
    nvram::mirrored_append(stream.flatten())?;
    flusher::schedule(id, pol.erasure_profile);
    Ok(())
}
```

**Zero-copy optimisation (Phase 3.1++)**  
The real pipeline mirrors this pseudocode while using `Cow<[u8]>` buffers end-to-end.  
Compression can hand back a borrowed slice when it deems the input "already optimal",  
and the write path hashes/encrypts that borrowed data directly. Only segments that go  
through LZ4/Zstd or encryption allocate fresh `Vec<u8>`, cutting per-segment copies  
and reducing large transfer latency by ~10-20% in internal benchmarks.

#### 5.1.1 Async coordination (`pipeline_async`)

- The optional `pipeline_async` feature swaps the single-threaded loop for a Tokio-based fan-out/fan-in pipeline. A bounded `Semaphore` (`max_concurrency`, default `num_cpus / 2`) caps in-flight segment work while a channel feeds the ordered commit loop.
- CPU-heavy steps (entropy check, compression, encryption) execute via `spawn_blocking`, returning `SegmentPrepared` records that carry preparation latency and the time they reached the coordinator.
- The coordinator uses `NvramTransaction` to stage all new segment writes. Durability happens once every segment succeeds; on error the transaction rolls back without touching disk and persistent dedupe increments are undone.
- `tracing` instrumentation records per-segment prep time, coordination delay, commit duration, and aggregated totals. `info!` summaries emit averages/maxima so CI can assert the <50 µs coordination target, while `trace!`/`debug!` provide per-segment detail when needed.
- Content registration is deferred until after the NVRAM transaction commits; staging dedupe hits rely on an in-memory map and adjust staged segment refcounts so intra-capsule dedupe remains deterministic.
- `PipelineConfig` exposes tuning knobs (`max_concurrency`, per-task memory limits, future transactional toggles) and is plumbed through `spacectl` so synchronous callers can opt in by enabling the Cargo feature.

### 5.2 Snapshot & Merkle root build

```rust
fn snapshot(volume: &Volume) -> SnapshotId {
    let snap_id = metadata::fork_tree(volume.id);
    merkle::seal_snapshot(snap_id); // hashes every new segment reference
    snap_id
}
```

### 5.3 Policy compilation (simplified)

```rust
fn compile_policy(pol: &PolicySpec) -> Vec<WorkflowStep> {
    use Action::*;
    let mut steps = Vec::new();
    if pol.replication.rpo == 0 {
        steps.push(MetroSync { dest: pol.replication.target });
    } else {
        steps.push(AsyncFanout { dest: pol.replication.target,
                                 max_lag: pol.replication.rpo });
    }
    if pol.snapshots.keep > 0 {
        steps.push(SnapshotSchedule { freq: pol.snapshots.freq,
                                      retain: pol.snapshots.keep });
    }
    steps
}
```

### 5.4 Autonomous repair loop

```rust
async fn health_agent_loop() {
    loop {
        let alerts = telemetry::fetch_degraded_media().await;
        for media in alerts {
            if media.isolate().is_ok() {
                rebuild::kick(media).await;
            }
        }
        sleep(Duration::from_secs(15)).await;
    }
}
```

---

## 6  Policy compiler & service mesh – sequence

```
+----------+        submit intent        +--------------+
| Operator | ───────────────────────────► |  API Server |
+----------+                               +------+------+
                                                │ create CRD
                                                ▼
                                         +------+------+
                                         | Policy Ctrl |
                                         +------+------+
                                                │ compile rules
                                                ▼
                                         +------+------+
                                         | Service Mesh|  (Envoy‑sidecars)
                                         +------+------+
                                                │ inject labels / mTLS certs
                                                ▼
                                         +------+------+
                                         | IO Engines  |
                                         +-------------+
```

---

## 7  Security & integrity

- TPM‑backed secure boot & node attestation.
- SPIFFE identities + mutual TLS enforced by eBPF gateway.
- Per‑segment XTS‑AES‑256 keys; envelope keys in external KMS.
- **Merkle tree per snapshot** for tamper proofing and ransomware roll‑back integrity.
- Post‑quantum ready (Kyber hybrid key wrapping selectable by policy).
- Immutable audit log (hash‑chained, external time‑stamp).
- Confidential compute job‑slots (SGX/SEV enclaves run WASM/Python on‑disk data).

---

## 8  Data protection & replication

| Mode                     | Description                     | RPO     | RTO       |
| ------------------------ | ------------------------------- | ------- | --------- |
| **6+2 EC**               | Intra‑cluster erasure coding    | 0       | < minutes |
| **Metro‑Sync**           | Intent‑log mirror via RDMA      | 0       | seconds   |
| **Async Fan‑out**        | Snapshot deltas → cluster/S3    | minutes | minutes   |
| **Namespace Federation** | Mount remote snapshot instantly | n/a     | seconds   |

---

## 9  Space efficiency techniques

| Feature            | Method                              | Runtime cost     |
| ------------------ | ----------------------------------- | ---------------- |
| Compression        | Entropy sample ⇒ LZ4 / Zstd / none  | < 1 µs/seg (DPU) |
| Deduplication      | 8 KB fingerprints, GPU bloom filter | negligible       |
| Snapshots & clones | Metadata redirect‑on‑write          | < 1 ms           |
| Tiering            | Heat counter, metadata move         | none             |

---

## 10  Hardware composability

```
+--------------+   hot‑plug  +-----------+
| Computational|───────────►| SPACE BUS |
|   SSD        |            +‑‑‑‑‑‑‑‑‑‑‑+
+--------------+                 │ register offload
                                 ▼
                          +------+-------+
                          | Offload Table|
                          +------+-------+
                                 │ same API used by CPU/DPU/GPU paths
                                 ▼
                          +------+-------+
                          | IO Engines   |
                          +--------------+
```

Feature parity is guaranteed: if an accelerator is absent, the CPU path runs the same Rust crate.

---

## 11  Edge & disconnected sites

- Single‑node build with embedded witness option.
- Gossip replication; reconciles once links restore.
- Eventual consistency toggle and local key escrow.

---

## 12  Autonomous repair & observation loop

```
[Telemetry Hub] ─► [Health Agent] ─► isolate media ─► trigger rebuild
      │                          ▲                      │
      └──► [ML Anomaly Detector]─┘<─ snapshot & lock ────┘
```

*Time‑to‑detect aims < 30 s; rebuild bandwidth bound by network not drive IO.*

---

## 13  Getting started (developer sandbox)

*Requirements:* Linux host, Rust 1.78+, Docker 25+, `kubectl`, 16 GB RAM.

```bash
# Bootstrap dev sandbox
git clone https://github.com/your‑org/space.git
cd space
make dev‑sandbox           # KIND cluster + local NVMe images
cargo build --workspace    # compile Rust crates
make run‑engines           # launch IO engines
spacectl volume create demo --size 50Gi --protocol nfs
```

---

## 14  Contribution & IP notice

This repository records **prior art** for the SPACE architecture.  By submitting pull requests you agree any contribution may be incorporated under Apache 2.0 and that **no patent licence** is granted unless explicitly stated.

Discussions via GitHub Issues; code submissions require a Developer Certificate of Origin.

---

© 2025 Shane Wall & contributors.  Licensed under the Apache License, Version 2.0.

