# CapsuleFlow Layout Engine

CapsuleFlow is the Phase 3.0 layout engine that compiles declarative storage policy into executable zoning kernels, keeps the pipeline thin, and exposes modular offloads (CPU, DPU, GPU, CSD) without forcing the entire stack to depend on pipeline internals.

## Architecture

1. **Policy Compiler** turns `Policy.layout` into a `LayoutOffload` implementation via feature-gated traits and helpers.
2. **Layout Engine** synthesizes a `ZonePlan` per capsule write, projecting deterministic IV seeds, segment references and optional Merkle roots for quantum-ready workflows.
3. **Offload Registry** routes the plan to the best hardware: CPU (default), DPU (RDMA), GPU (`tch`), or computational SSD (`libzbd`).

```
[Policy Compiler] ? [LayoutEngine] ? [Offload Registry]
                                    +- CPU
                                    +- DPU (RDMA)
                                    +- GPU (tch-rs)
                                    +- CSD (libzbd)
```

## Threat Model

- **Policy enforcement** is deterministic: compilation and zone planning happen inside the trusted runtime.
- **Data layout** uses deterministic IV seeds per zone to maintain dedupe-friendly encryption while resisting replay attacks.
- **Execution isolation** is maintained by keeping ML inference localized to sandboxed offloads (`tch`/libtorch) and deferring ML feature gates.

## Performance Model

- CPU baseline must handle 80 µs synthesis by emitting simple fixed 4 MiB zones.
- GPU offload (TorchScript) should hit ~15 µs by evaluating heat histograms and telemetry vectors.
- ZNS layout engine maps adjacency graphs to zone lists, growing linearly with capsule count but constant with scheduler depth.

## Open Questions

- TorchScript model format and telemetry exporters.
- ZNS device discovery and configuration (`--zns-dev`).
- Real PQ crypto anchoring (SPHINCS+ or `pqcrypto` bridge).
