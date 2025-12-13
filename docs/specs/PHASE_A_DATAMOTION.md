# Phase A: The DataMotion Unification (Tactical Fix)

- **Target Component:** `crates/scaling`
- **Status:** Approved for implementation
- **Priority:** P0
- **Related:** `docs/podms.md`, `crates/scaling/src/agent.rs`

## Executive Summary
The ScalingAgent had a split brain: migration streamed real data, but replication was a stub. Phase A fixes this by extracting the working transport logic into a shared **DataMotion** engine. Both metro-sync replication and migrations now invoke the same pipeline so Zero-RPO protection is real code, not intent logging.

## DataMotion Primitive
- **Intent:** Copy (replication/backup) vs Move (migration/evacuation/tiering).
- **Flow:** Acquire from NvramLog → optional transform (decrypt/decompress/re-compress/re-encrypt via `SwarmOps`) → fan-out transport → finalize (delete on Move, register success on Copy).
- **Context:** `DataMotionContext` bundles `MeshNode`, `CapsuleCatalog`, `NvramLog`, and optional `KeyManager` to keep signatures tight and share dependencies.

### Engine API
```rust
pub enum MotionMode { Copy, Move }

async fn execute_data_motion(
    &self,
    capsule_id: CapsuleId,
    targets: Vec<NodeId>,
    mode: MotionMode,
    transform: bool,
    reason: &str,
) -> Result<usize>;
```

### Behaviors
- **Validation:** Lookup capsule from catalog and enforce sovereignty across all targets.
- **Transforms:** Preserve existing migration pipeline (MAC validation, decrypt → decompress → recompress → re-encrypt, key rotation when metadata is absent).
- **Transport:** Parallel fan-out of `ReplicationFrame`s to every target with join-set acknowledgement.
- **Finalize:** Copy leaves source intact; Move deletes source segments and removes the capsule metadata after successful delivery.

## Wiring Metro-Sync
`execute_metro_sync_replication` now selects peers (or honors caller-specified targets) and delegates to `execute_data_motion` in `Copy` mode. Replication is no longer a placeholder; it uses the same data path as migrations with consistent crypto and dedup semantics.

## Testing
- **`crates/scaling/tests/data_motion_test.rs`**
  - `data_motion_copy_preserves_source`: Copy keeps source data while streaming to the peer.
  - `data_motion_move_cleans_source`: Move cleans local state after successful delivery.

## Changelog Notes
- Added unified `DataMotion` engine in `ScalingAgent`.
- Metro-sync replication now streams real payloads through DataMotion instead of logging intent.
- Migration path generalized over `MotionMode` to cover both copy and move semantics with identical security and transform logic.
