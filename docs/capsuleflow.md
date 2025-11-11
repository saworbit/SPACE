---
id: capsuleflow
title: CapsuleFlow Layout Engine
---

# CapsuleFlow Layout Engine

CapsuleFlow is the Phase 3.0 layout engine for SPACE. It builds zone plans from policy intent, materializes deterministic IV seeds, and ships work to the optimal offload (CPU, DPU, GPU, or computational SSD) without bloating the write pipeline crate.

## Key Concepts

- **Policy-compiled layout:** `Policy.layout` drives the compiler, which instantiates a `LayoutOffload` implementation. This keeps the write pipeline thin—its job is now just to call into `LayoutEngine::synthesize`.
- **ZonePlan outputs:** Each synthesis produces `zones`, deterministic `iv_seed`s, and optional PQ-capable `merkle_root`s that later flow into the encryption/dedup path.
- **Feature gating:** CPU fallback (`CpuFixed`) is always on. ZNS graphs, Torch-based layouts, and post-quantum anchors are gated behind `zns`, `ml`, and `pq` features so we can test the standard path without needing libzbd or libtorch.
- **Hardware offload registry:** The compiler dispatches to a `LayoutOffload` trait object, so DPUs, GPUs, or CSDs can register without touching the pipeline crate.

## Behavior

1. **Policy evaluation → LayoutPolicy.** Policies acquired from protocol containers carry a `LayoutPolicy` with strategy, EC profile, and heat thresholds.
2. **LayoutEngine invocation.** `pipeline::WritePipeline::write_capsule` builds `data_slices` and calls `LayoutEngine::synthesize`, which forwards to the compiled offload.
3. **Segment-level flow.** The returned `ZonePlan` feeds the compression/dedup/encryption loop instead of chunking blindly.
4. **Optional accelerators.** Torch-based logic generates layouts via `tch` when `ml` is enabled, while `zns` lets `libzbd` drive zone append calls. Both are invisible unless specifically enabled.

## Diagnostics

- Metricize zone plans: note counts, merkle roots, and runtime (target <80 µs CPU, <15 µs GPU).
- Log the offload used per capsule so hardware swaps appear in observability feeds.
- Validate `zone_plan.merkle_root` when `Policy.layout.strategy == QuantumReady`.

## Next Steps

1. Provide a TorchScript model for the `Learned` strategy and publish docs for training telemetry vectors.
2. Extend the ZNS graph planner with `libzbd::Zone::append()` integration and fallback for non-ZNS devices.
3. Hook GPU/DPU diagnostics into the policy compiler registry so we can monitor offload utilization per write.
