# SPACE v0.2 — Core Capsule MVP Scope

> **Last updated:** 2026-05-07
>
> This document defines what ships in v0.2 ("Core Capsule") and what is
> explicitly deferred. It is the single authoritative scope contract for the
> next release. If something is not listed under **In Scope**, it is out.

---

## Goal

Deliver a **shippable, testable, single-node artifact** that proves the core
thesis: *one capsule, multiple views, with compression + dedup + encryption
working together transparently.*

Success criteria: a user can `cargo install spacectl`, store objects via S3 or
CLI, get automatic compression + dedup + encryption, run background GC, and
observe the system via metrics — all on one node, with no feature flags
required.

---

## In Scope (v0.2)

### Core Storage
- Stable single-node capsule CRUD (create / read / delete / stream / range)
- Write pipeline: segment -> compress -> hash -> dedup -> encrypt -> store
- Read pipeline: read -> verify MAC -> decrypt -> decompress -> assemble
- Modular pipeline as the **default** path (remove legacy bridge)
- Automatic background GC with tunable aggressiveness (replaces manual-only GC)
- Reference counting that actually reclaims space

### S3 Protocol View
- Complete basic operations: PUT, GET, HEAD, LIST, DELETE
- Multipart upload support
- Streaming uploads/downloads (no full buffering)
- Range request support
- Proper error mapping to S3 error codes
- Encryption-transparent: plaintext to clients, encrypted on disk

### CLI (`spacectl`)
- `create`, `read`, `delete`, `list` — polished with progress bars
- `serve-s3` — start the S3 gateway
- `doctor` / `validate` — check key material, disk space, permissions
- Human-readable output by default, `--json` flag for scripting
- Config file support (`~/.config/space/config.toml` or env vars)
- Shell completions (bash, zsh, PowerShell)

### Observability
- Prometheus metrics: per-stage latency histograms, dedup hit rate, GC
  effectiveness, storage utilization, cache hit rate
- Structured tracing with `RUST_LOG` control
- Deep health checks with actionable output
- `spacectl status` command

### Testing & Quality
- Property-based tests (proptest) for pipeline invariants:
  round-trip correctness, dedup semantics under encryption, MAC verification
- Criterion benchmarks for the hot path (segment pipeline) tracked in CI
- End-to-end integration tests via the S3 interface
- All existing tests passing

### Documentation
- README split: short "What you can use today" + link to full vision doc
- Feature status table as single source of truth (machine-readable YAML)
- Every rustdoc page marks "Implemented & Tested" vs "Experimental"

---

## Explicitly Deferred (post-v0.2)

These items are valuable but out of scope for v0.2. They move behind
`experimental` feature flags or into `experimental/` sub-crates.

| Category | Item | Notes |
|----------|------|-------|
| **Distributed** | Full PODMS mesh / gossip / swarm intelligence | Keep behind `podms` flag |
| **Distributed** | Raft consensus for metadata | Keep experimental |
| **Distributed** | Metro-sync / async replication | Keep behind `podms` flag |
| **Distributed** | Cross-node dedup | Depends on mesh stability |
| **Protocol** | NFS export | Experimental, minimal usage |
| **Protocol** | Block volume / NVMe-oF target | Experimental |
| **Protocol** | FUSE filesystem | Experimental, Unix-only |
| **Protocol** | CSI driver | Stub only |
| **Security** | SPIFFE / mTLS / eBPF gateway | Keep behind `advanced-security` |
| **Security** | Post-quantum crypto (Kyber) | Untested, keep gated |
| **Security** | Counting Bloom filters | Keep behind `advanced-security` |
| **Compute** | WASM transform engine | Phase 5 |
| **Compute** | Layout engine / ML placement | Future |
| **Infra** | Web interface / dashboard | Not needed for CLI-first MVP |
| **Infra** | Federation / sharding | Depends on distributed layer |

---

## Strategic Roadmap

### Phase 0: Stabilization Sprint (now - 1 month)

- [ ] Align all documentation with current reality
- [ ] Consolidate 3-5 crates (merge compression + dedup + encryption into
      `pipeline-core` or keep separate but tiny; fold tiering into storage)
- [ ] Make modular pipeline the default; remove legacy bridge
- [ ] Implement automatic background GC
- [ ] Add property-based tests + basic Criterion benchmark harness
- [ ] Lock the core API surface
- [ ] Feature flag hygiene: document "stable" vs "research" feature sets
- [ ] Add CI matrix that only tests supported feature combinations

### Phase 1: Core Capsule v0.2 Release (1-4 months)

- [ ] Production-grade single-node experience
- [ ] S3 view: multipart, range requests, proper error codes
- [ ] CLI polish: progress bars, `doctor` command, config file, completions
- [ ] Expanded Prometheus metrics (per-stage histograms, dedup/GC stats)
- [ ] Published benchmark numbers from CI
- [ ] Property-based + integration test coverage
- [ ] Clear "how to contribute" and good-first-issues list
- [ ] External security review of the encryption crate (scheduled)

### Phase 2: Selective Distributed Layer (after Phase 1 is solid)

- [ ] Only after Phase 1 has real usage and feedback
- [ ] Simplify initial distributed story: Raft for metadata + simple async
      replication before full gossip + PODMS
- [ ] Consider spinning mesh/PODMS into a separate experimental repo
- [ ] Re-evaluate NFS/Block/FUSE views based on user demand

---

## Crate Consolidation Plan

Current state: ~25 workspace members. Target: reduce cognitive and build overhead.

| Action | Crates Affected | Rationale |
|--------|----------------|-----------|
| **Merge into `pipeline-core`** | `compression`, `dedup`, `encryption` | Tightly coupled, always used together |
| **Fold into `storage`** | `tiering` | Tiering is a storage concern |
| **Move to `experimental/`** | `transform-engine`, `layout-engine` | Not needed for v0.2 |
| **Keep as-is** | `common`, `capsule-registry`, `pipeline`, `spacectl`, `protocol-s3`, `storage`, `nvram-sim`, `foundry` | Core surface area |
| **Evaluate** | `gossip-layer`, `mesh-core`, `podms-orchestrator`, `scaling`, `federation` | Bundle under single `distributed` crate or move to separate repo |

---

## Tactical Next Steps (pick 3-5 to start)

1. **This document** — share with potential contributors for feedback
2. Run a crate consolidation spike (merge 2-3 small crates, measure build time)
3. Add a CI job that validates the feature status table against code
4. Implement background GC + make it the default
5. Write 5-10 property-based tests for pipeline invariants
6. Polish `spacectl` with one high-value UX improvement (progress + errors)
7. Schedule a focused security review of the encryption crate
