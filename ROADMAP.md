# SPACE Roadmap

> **Last updated:** 2026-05-07
>
> This is the canonical roadmap for SPACE. It is split into three horizons:
> **Phase 0** (now), **Phase 1** (v0.2 release), and **Phase 2** (post-v0.2).
> Detailed scope for the upcoming release lives in [MVP_SCOPE.md](MVP_SCOPE.md).
> Long-term vision lives in [docs/future_state_architecture.md](docs/future_state_architecture.md).

---

## Guiding Principles

1. **Ship a working single-node artifact first.** The core thesis ("one
   capsule, multiple views, with compression + dedup + encryption coexisting")
   is provable on one node. Prove it before scaling out.
2. **Ruthless prioritization.** Single-developer velocity demands a small
   surface area. Distributed/experimental features stay behind feature flags
   until v0.2 ships and gets real usage.
3. **Documentation must match reality.** Every claim of "implemented" should
   correspond to tested code. The [Feature Status Table](README.md#-feature--capability-status)
   is the single source of truth.
4. **Build trust through benchmarks and audits.** Published numbers from CI;
   external security review of the encryption crate before any production
   conversation.

---

## Where We Are Today

**Maturity:** Pre-alpha (v0.1.0)

**Solid (🟢 Beta):**
- Capsule storage, metadata registry (Sled), NVRAM log
- LZ4/Zstd compression with entropy detection
- BLAKE3 content-addressed deduplication
- XTS-AES-256 encryption with deterministic tweaks (preserves dedup)
- BLAKE3-MAC integrity verification
- Foundry block storage (LegacyBackend, MagmaBackend, snapshots, chain replication)
- Background scrub executor with light/deep modes
- Byte-bounded LRU read cache (`CachedBackend`)

**Working but rough (🟡 Alpha):**
- S3 REST API (basic ops only — no multipart, limited error mapping)
- `spacectl` CLI (functional but UX rough)
- Manual GC (not automatic)
- Key management with rotation
- Prometheus metrics (5 counters/gauges)
- Raft control plane (Phase 9.1-9.4)

**Experimental (🟠):**
- PODMS mesh, gossip, metro-sync replication, scaling agents
- NFS/Block/NVMe-oF/FUSE/CSI protocol views
- SPIFFE/mTLS/eBPF gateway, post-quantum crypto
- Web interface, federation gRPC bridge
- WASM transform engine (Phase 5)

**Reality check:** ~25 workspace crates is too many for a single developer.
Many features are proofs-of-concept that drag on build times, test surface,
and cognitive load. The next release tightens this.

---

## Phase 0 — Stabilization (now → ~1 month)

**Goal:** Lock the foundation. Align docs with reality, reduce surface area,
and make the modular pipeline the default.

### Documentation
- [x] Create [MVP_SCOPE.md](MVP_SCOPE.md) and [ROADMAP.md](ROADMAP.md)
- [x] Update README to reference v0.2 scope
- [x] Mark [future_state_architecture.md](docs/future_state_architecture.md) as vision-only
- [ ] Audit every "implemented" claim in docs against the code
- [ ] Convert the Feature Status Table to machine-readable YAML for CI
- [ ] Add a CI check that fails if status table and code reality diverge significantly
- [ ] Split README into "What you can use today" + link to vision

### Crate Consolidation
- [ ] Spike: merge `compression` + `dedup` + `encryption` into `pipeline-core` (or document why they should stay separate). Measure build-time impact.
- [ ] Fold `tiering` into `storage`
- [ ] Move `transform-engine` and `layout-engine` to `experimental/`
- [ ] Evaluate bundling `gossip-layer` + `mesh-core` + `podms-orchestrator` + `scaling` + `federation` into a single `distributed` crate (or move to a separate repo)
- [ ] Document the post-consolidation crate map in [ARCHITECTURE.md](ARCHITECTURE.md)

### Pipeline & Feature Flags
- [ ] Make `modular_pipeline` the default path
- [ ] Remove the legacy bridge after a deprecation cycle
- [ ] Document "stable feature set" vs "research feature set"
- [ ] CI matrix tests only the supported feature combinations
- [ ] Default release builds are minimal (no `phase4`, no `podms`, no `advanced-security`)

### Quality
- [x] **Property-based tests (proptest) for pipeline invariants** — `crates/capsule-registry/tests/proptest_pipeline.rs`:
  - Round-trip correctness (write → read = original), unencrypted and encrypted
  - Dedup determinism (same payload N times → identical segment lists, `ref_count == N`)
  - Content separation (distinct payloads → distinct segment lists)
  - **No cross-policy dedup collision** (raw LZ4 frame under `None` policy vs plaintext under `LZ4` policy must not share segments)
  - Boundary sizes (empty, 1 byte, 4 MiB ± 1)
  - **Caught three real bugs** — see CHANGELOG `[Unreleased]`: data-loss in legacy read path for uncompressed segments, XTS-AES sub-block plaintext gap, and a cross-policy dedup-key collision found by code review of the first fix
- [ ] Additional invariants to add later: MAC verification fails on tampered ciphertext, compression entropy detection skips random data
- [ ] Basic Criterion benchmark harness for the segment write hot path
- [ ] Lock the core API surface; document breaking-change policy

---

## Phase 1 — Core Capsule v0.2 Release (1 → 4 months)

**Goal:** A shippable single-node artifact that proves the core thesis.

A user should be able to: install `spacectl`, store objects via S3 or CLI,
get automatic compression + dedup + encryption, run background GC, and
observe the system via metrics — all on one node, with no feature flags
required.

### Core Storage
- [ ] Stable single-node capsule CRUD (create / read / delete / stream / range)
- [ ] **Automatic background GC** with tunable aggressiveness (replaces manual-only)
- [ ] Reference counting that actually reclaims space
- [ ] Strengthen error handling around Raft proposals and NVRAM appends
- [ ] Structured error types with recovery guidance

### S3 Protocol View
- [ ] **Multipart upload** support
- [ ] **Range requests** (`GET` with byte ranges)
- [ ] Streaming uploads/downloads (no full buffering)
- [ ] Proper error mapping to S3 error codes
- [ ] Lifecycle policy stubs
- [ ] **Encryption-transparent**: plaintext to clients, encrypted on disk
  (already designed via `RegistryTransformOps` + `enforce_view_policy`)

### CLI (`spacectl`) Polish
- [ ] Progress bars for large operations
- [ ] `spacectl doctor` — checks key material, disk space, permissions
- [ ] Config file support (`~/.config/space/config.toml` or env)
- [ ] Human-readable output by default; `--json` flag for scripting
- [ ] Shell completions (bash, zsh, PowerShell)
- [ ] Clearer error messages

### Observability
- [ ] Per-stage latency histograms (compress, hash, encrypt, MAC, write)
- [ ] Dedup hit rate metric
- [ ] GC effectiveness metric (segments reclaimed, bytes freed)
- [ ] Cache hit rate metric
- [ ] Storage utilization metric (`used_bytes()` is already in the trait)
- [ ] Deep health checks with actionable output
- [ ] `spacectl status` command

### Testing & Benchmarks
- [ ] Property-based tests for pipeline invariants (carried from Phase 0)
- [ ] Criterion benchmarks tracked in CI with regression alerts
- [ ] End-to-end integration tests via the S3 interface
- [ ] Published benchmark numbers in the README
- [ ] Real measurements from the simulation environment (not just micro-benches)

### Security
- [ ] **External security review** of the `encryption` crate (scheduled)
- [ ] Document threat model
- [ ] Fuzz harnesses for `encrypt_roundtrip`, `decompress`, S3 path parsing

### Release Engineering
- [ ] Version bump to `0.2.0`
- [ ] Tagged release on GitHub
- [ ] Pre-built binaries for Linux + Windows
- [ ] Docker image published to GHCR
- [ ] Migration notes from `0.1.x`

---

## Phase 2 — Selective Distributed Layer (post-v0.2, only after real usage)

**Goal:** Add multi-node capability *only after* Phase 1 has real users and
feedback. No upfront commitment to the full PODMS vision until we know what
users actually need.

### Simplified Distributed Story First
- [ ] Raft for metadata (already partially done — Phase 9)
- [ ] Simple async replication (single source of truth + followers)
- [ ] Defer the full gossip + PODMS swarm until simpler approaches are proven
  insufficient

### Re-evaluate Experimental Features
- [ ] PODMS mesh — keep or move to a separate experimental repo?
- [ ] NFS / Block / FUSE / CSI views — which ones have user demand?
- [ ] WASM transforms — is there a real workload pulling for this?

### Operational Maturity
- [ ] Backup and restore tools
- [ ] Data migration tools
- [ ] Upgrade/downgrade story
- [ ] Multi-node failover validation (chaos testing)

---

## Long-term Vision (post-Phase 2)

These remain in the vision but are not committed to any release timeline.
See [docs/future_state_architecture.md](docs/future_state_architecture.md)
for the full narrative.

- Full mesh federation with policy-orchestrated mobility (PODMS)
- ML-driven placement and adaptive policy synthesis
- Hardware offload (DPU, GPU, computational SSDs)
- Confidential compute integration (SGX/SEV enclaves)
- Post-quantum cryptography hardened and audited
- Erasure coding with pluggable algorithms
- Full Kubernetes integration (production CSI driver)
- Cross-zone routing and global namespace

---

## Tactical Next Steps (next 2-4 weeks)

Pick 3-5 from this list to make immediate progress:

1. ✅ **Publish [MVP_SCOPE.md](MVP_SCOPE.md) and ROADMAP.md** — done; share for feedback
2. [ ] **Crate consolidation spike** — merge two small crates and measure
3. [ ] **CI status-table check** — fail builds when docs and code drift
4. [ ] **Background GC** — make automatic GC the default
5. [x] **Property-based tests** — 4 proptest cases + 5 boundary tests covering pipeline invariants (caught a data-loss bug on the legacy read path)
6. [ ] **`spacectl` UX** — pick one high-value polish item (progress bars or `doctor`)
7. [ ] **Schedule security review** — reach out about an encryption-crate audit

---

## How to Influence the Roadmap

- Open a GitHub Discussion to debate priorities
- File issues tagged `roadmap` to suggest changes
- Submit PRs that advance the v0.2 goals listed above (highest priority)
- Experimental work is welcome but must stay behind feature flags

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full contribution process.
