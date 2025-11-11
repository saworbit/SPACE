# SPACE Future State Architecture

## Executive Summary

SPACE redefines the relationship between storage, compute, and orchestration. It introduces the **capsule** as a universal data primitive that erases the distinction between block, file, and object while preserving cryptographic verifiability and policy control. Built entirely as a collection of stateless microservices written in Rust, SPACE unifies data access, policy enforcement, and self-repair within a single programmable substrate.

This paper outlines the guiding philosophy, architectural blueprint, and reasoning behind SPACE. It serves as both a north star for contributors and a technical narrative for partners and investors. The core message is simple: **data infrastructure should think for itself**—adapting to context, enforcing intent, and proving integrity at every step.

*A vision paper outlining the evolving architecture, design rationale, and guiding principles of the Storage Platform for Adaptive Computational Ecosystems (SPACE).*

---

## 1. Purpose and Philosophy

SPACE exists to unify the principles of **data mobility, compute proximity, and autonomous resilience** under one modular fabric. Modern enterprises are increasingly distributed—from sovereign clouds to GPU accelerators and air-gapped environments—but their data systems remain fractured. Traditional storage architectures evolved around protocols, not intelligence.

SPACE starts over. Every entity is an object; every service is a container; every decision is policy-driven. It treats **data as code**, applying compile-time reasoning to runtime infrastructure. This document captures the future-state architecture, rationale, and evolution path of SPACE as an adaptive, policy-defined data fabric.

---

## 2. Design Tenets

| Tenet | Description |
|-------|--------------|
| **Composable Intelligence** | Every subsystem (storage, compute, or metadata) operates as a plug-in service. Intelligence can run anywhere—CPU, DPU, GPU, or computational SSD. |
| **Zero-Trust Everywhere** | SPIFFE identities, mTLS, TPM attestation, and per-segment encryption are foundational, not optional. |
| **Policy-Driven Operation** | Operators express intent declaratively; SPACE compiles it into executable workflows. No manual pipelines. |
| **Self-Repairing Fabric** | Agents isolate and rebuild failures before they escalate into outages. Telemetry is not passive—it acts. |
| **Capsule-first universal namespace** | One 128-bit CapsuleId is the single source of truth. Views are projected as block, file, or object without copying or conversion. Everything speaks the same language. |
| **Sovereign by Design** | The fabric functions in connected or disconnected states, maintaining full local authority and integrity. |

---

## 3. Context and Motivation

The modern data plane is no longer bounded by datacentres. It stretches across air-gapped defence networks, confidential compute enclaves, and multi-cloud edges. These domains require:

- **Composable performance** (scale CPU, DPU, and GPU independently)
- **Cryptographic sovereignty** (prove who, what, when, and where every operation occurred)
- **Runtime introspection** (learn from workloads and adapt automatically)

SPACE addresses these by collapsing storage, compute, and orchestration into a **single programmable substrate** that can exist across trust zones and geographies.

---

## 4. High-Level Architecture

```
                   +----------------------- CONTROL PLANE -----------------------+
                   | Operators | Policy | Service | Telemetry | CLI / GraphQL  |
                   |  (CRDs)   |Compiler|  Mesh   |  Hub      | gRPC / REST    |
                   +----+------+---+----+----+----+-----+-----+------+----------+
                        |          |         |          |           |
+-----------------------v----------v---------v----------v-----------v-----------+
|                           SERVICE   MESH  (mTLS)                              |
+------------+-----------+-----------+------------+-----------+-----------------+
             |           |                        |           |
             v           v                        v           v
   +---------+--+  +-----+-----+          +-------+----+  +---+----+
   |  CSI       |  |  NFS/SMB |          |   S3      |  | NVMe-oF |
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
|         METADATA  MESH  (FoundationDB-style KV, Paxos)                        |
+--------------------+--------------------------+-------------------------------+
                     |                          |
                     v                          v
              +------+-------+            +-----+-------------------+
              | CapsuleRegistry|          |  NVRAM Log / Flash /    |
              | (maps Capsule →|          |  Disk tiers             |
              |  Segments)     |          +-------------------------+
              +----------------+
```

### 4.1 Capsules: the universal primitive

A **capsule** is the only stored entity. It owns a CapsuleId, a list of SegmentIds, and view attributes. Protocols do not store separate copies; they **project** views over the same capsule.

```
+-------------------------+      view(Block)  → NVMe-oF LBA ranges
| Capsule {               |      view(File)   → POSIX path + attributes
|   id: CapsuleId,        |      view(Object) → S3 key + headers
|   segments: [SegmentId],|
|   size: u64,            |
|   labels: Map<K,V>      |
| }                       |
+-------------------------+
```

Pseudocode for view projection:

```rust
pub enum Protocol { Block, File, Object }

pub fn open_view(id: CapsuleId, p: Protocol) -> Result<View> {
    let meta = registry.lookup(id)?;
    match p {
        Protocol::Block  => Ok(View::Block(BlockView::attach(meta))),
        Protocol::File   => Ok(View::File(FileView::attach(meta))),
        Protocol::Object => Ok(View::Object(ObjectView::attach(meta))),
    }
}
```

Rationale: using a single primitive removes protocol silos, reduces migration effort, and enables consistent policy enforcement across all access paths.

---

## 5. Key Components

### 5.0 Capsule Registry
- Authoritative index mapping `CapsuleId → [SegmentId]`, size and labels.
- Backed by the metadata mesh for consistency and durability.
- First-class API used by write and read pipelines.

### 5.1 Control Plane

### 5.1 Control Plane
- Built as a set of Kubernetes CRDs with a **policy compiler** that converts declarative intents into operational workflows.
- Telemetry Hub aggregates performance, health, and security signals.
- REST and GraphQL endpoints expose live topology, status, and metrics.

### 5.2 Service Mesh
- Uses Envoy-based sidecars for encrypted service-to-service communication.
- Injects SPIFFE identities automatically into each container.
- Supports dynamic re-routing of IO traffic based on policy tags (tenant, QoS, or hardware affinity).

### 5.3 IO Engines
- Stateless Rust + SPDK components responsible for compression, encryption, and replication.
- Each engine can be migrated in seconds across hosts or DPUs.
- Engines communicate via RDMA or gRPC using zero-copy buffers.

### 5.4 Metadata Mesh
- Strongly consistent FoundationDB-like KV store.
- Sharded by object ID range, with Paxos coordination.
- Maintains per-segment fingerprints, encryption keys, and policy bindings.

### 5.5 eBPF Policy Gateway
- Enforces identity, QoS, and segmentation in kernel space before IO enters user space.
- Hooks at TC ingress verify SPIFFE certificates and assign traffic to cgroups.

### 5.6 Offload Framework
- Hardware-agnostic trait registry allowing the same crate to execute on CPU, DPU, or GPU.
- Dynamic registration and hot-unplug supported.

```rust
trait Offload {
    fn checksum(&self, buf: &[u8]) -> u32;
    fn compress(&self, seg: &[u8]) -> Vec<u8>;
    fn erasure_encode(&self, stripe: &[Segment]) -> Vec<Parity>;
}
```

---

## 6. Data Lifecycle

### 6.1 Write Path

```rust
fn write_object(id: Uuid, data: &[u8], pol: &Policy) -> Result<()> {
    let segments = segmenter::split(data, 4 * MIB);

    let stream = segments.into_iter().map(|seg| {
        let seg = compressor::adaptive(seg);
        if dedupe::is_duplicate(&seg)? { return Ok(None) }
        let ciphertext = crypto::encrypt_xts(seg, keyring::derive(&id));
        Ok(Some(ciphertext))
    });

    nvram::mirrored_append(stream.flatten())?;
    flusher::schedule(id, pol.erasure_profile);
    Ok(())
}
```

### 6.2 Read Path

```
[App] ➔ [Protocol Container] ➔ [eBPF Gateway] ➔ [IO Engine]
                                             |
                                             ├─ cache hit (NVRAM)
                                             ├─ flash hit (direct)
                                             └─ disk / cold tier → promote
```

### 6.3 Autonomous Repair Loop

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

## 7. Security and Verification Model

- **TPM-backed secure boot** for node attestation.
- **SPIFFE/mTLS** at every boundary.
- **Per-segment AES-XTS encryption** with deterministic IVs enabling dedupe over ciphertext.
- **Merkle tree snapshots** seal lineage per volume, anchoring hashes to rotating KEKs.
- **Immutable audit log** with hash-chained events and external timestamping.
- **Confidential compute job slots** for WASM or Python analytics within SGX/SEV enclaves.

---

## 8. Adaptive Intelligence Layer

SPACE integrates continuous telemetry and reinforcement learning to tune itself:

- Predictive capacity and latency modelling.
- Self-tuning compression and erasure profiles.
- Health agents that detect anomalies, preemptively isolate hardware, and trigger rebuilds.
- Policy feedback loops that rewrite QoS or replication rules when real-world conditions deviate.

*In effect, SPACE behaves like a distributed organism: self-healing, observant, and context-aware.*

---

## 9. Edge and Sovereign Deployments

- Single-node configurations with embedded witness and gossip replication.
- Eventual-consistency toggle for intermittent connectivity.
- Local key escrow ensuring sovereignty even without access to central KMS.
- Suitable for classified, defence, and remote research environments.

---

## 9.1 Capsule Lifecycle and API Sketch

A capsule progresses through clear stages of existence, each bound to verifiable metadata. The lifecycle enables fine-grained introspection and policy enforcement.

### Lifecycle Overview
```
[create_capsule] → [segment_data] → [append_to_nvram] → [flush_to_erasure_tier] → [register_in_metadata_mesh] → [open_view]
```

### API Sketch
```rust
fn create_capsule(labels: Map<String, String>) -> CapsuleId {
    registry::allocate(labels)
}

fn write_capsule(id: CapsuleId, data: &[u8]) -> Result<()> {
    pipeline::segment_and_store(id, data)
}

fn read_range(id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
    pipeline::fetch(id, offset, len)
}
```

### Design Implications
- Capsules form immutable, audit-friendly boundaries for data ownership.
- They allow mixed-protocol access without duplication.
- Capsule metadata embeds lineage, labels, and segment hashes for cryptographic verification.

---

## 10. Design Rationale and Future Direction

### 10.1 Why This Architecture Works
- **Stateless microservices** remove upgrade downtime and protocol lock-in.
- **Declarative policies** turn human processes into reproducible infrastructure logic.
- **Unified namespace** simplifies data mobility across heterogeneous workloads.
- **Hardware composability** aligns with the trend toward DPUs and computational storage.
- **Confidential compute integration** future-proofs against regulatory and privacy demands.

### 10.2 Looking Forward
- Expand the Policy Compiler into a full **constraint solver** that weighs latency, RPO, and energy cost.
- Integrate **AI-assisted policy synthesis** to recommend optimal data placement.
- Extend the **offload framework** to quantum-resistant cryptographic modules.
- Formal verification of **Merkle root integrity proofs** across federated clusters.

### 10.3 Phase 4: View Federation Layer
- **Protocol adapters** (`protocol-nvme`, `protocol-nfs::phase4`, `protocol-fuse`, `protocol-csi`) expose NVMe, NFS/FUSE, and CSI views from the same capsule namespace.
- **Mesh federation** (MeshNode resolve/federate/shard helpers) keeps metadata sharded across zones and returns federated targets in ≤100µs.
- **Policy signals** (latency_target, sovereignty) orchestrate view projection, transformation, and migration, ensuring QoS and compliance without copying data.
- Reference [docs/phase4.md](docs/phase4.md) for the complete Phase 4 implementation picture, governance docs, and test harness scripts.

---

### Authors
**Shane Wall**  
Principal Architect, SPACE Project  

---

© 2025 Shane Wall & contributors. Licensed under the Apache License, Version 2.0.
