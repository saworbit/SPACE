# Inbound Replication Implementation Status

## Summary
This document tracks the implementation of fixing the inbound replication data discard issue in the SPACE project.

## Problem Statement
The mesh listener spawns `handle_mirror_connection()` which currently reads incoming replication streams into a buffer but discards the data without:
- Validation (MAC integrity checks)
- Persistence (writing to NvramLog)
- Deduplication (checking ContentHash)
- Metadata updates (CapsuleRegistry updates)

This renders replication a no-op, undermining data durability and PODMS policy-driven scaling.

## Implementation Progress

### ✅ Completed Tasks

1. **Dependencies Added** ([crates/scaling/Cargo.toml](../../crates/scaling/Cargo.toml))
   - Added `bytes` crate for efficient buffer management
   - Added `blake3` for content hashing
   - Added `bincode` for wire protocol serialization
   - Added `encryption` crate for crypto operations
   - Added `nvram-sim` for NVRAM log access

2. **Wire Protocol Defined** ([crates/scaling/src/replication.rs](../../crates/scaling/src/replication.rs))
   - Created `ReplicationFrame` struct with:
     - Segment ID
     - Encryption metadata (key version, tweak, MAC tag)
     - Encrypted segment data
   - Implemented length-prefixed framing (4-byte length + bincode payload)
   - Added serialization/deserialization methods

3. **Replication Handler Implemented** ([crates/scaling/src/replication.rs](crates/scaling/src/replication.rs))
   - Created `ReplicationHandler<C: CapsuleCatalog>` generic over content catalog
   - Implemented full inbound flow:
     - Frame length/data reading with bounds checking (16MB max)
     - MAC validation using `encryption::mac::verify_mac()`
     - XTS-AES-256 decryption using `encryption::xts::decrypt_segment()`
     - BLAKE3 content hash computation
     - Deduplication check via `CapsuleCatalog::lookup_content()`
     - Refcount increment for dedup hits
     - NvramLog persistence for new segments
     - Content registration via `CapsuleCatalog::register_content()`
   - Added comprehensive logging (debug, info, warn, error)

4. **Integration with MeshNode** ([crates/scaling/src/lib.rs](../../crates/scaling/src/lib.rs))
   - Made `MeshNode` generic over `CapsuleCatalog` implementation
   - Updated `MeshNode::new()` to accept:
     - `Arc<C>` (CapsuleCatalog implementation)
     - `Arc<RwLock<NvramLog>>`
     - `Arc<RwLock<KeyManager>>`
   - Created `ReplicationHandler` instance in `MeshNode::new()`
   - Updated `start_mirror_listener()` to spawn handler tasks

### ⚠️ Remaining Issues

**Compilation Errors:**

1. **Cyclic Dependency Removed**: Successfully avoided circular dependency between `scaling` and `capsule-registry` by using the `CapsuleCatalog` trait from `common::traits`

2. **Generic Parameter Propagation** ([crates/scaling/src/agent.rs](../../crates/scaling/src/agent.rs))
   - `ScalingAgent` references `MeshNode` without generic parameter
   - Need to make `ScalingAgent` generic: `ScalingAgent<C: CapsuleCatalog>`
   - Update all usages in agent.rs (lines 27, 33, 41, 338, 349)
   - Update test code to provide concrete type

3. **Unused Variables** ([crates/scaling/src/replication.rs](crates/scaling/src/replication.rs))
   - Line 254: unused `mut` on `nvram_log`
   - Line 272: unused `mut` on `nvram_log`
   - Minor warnings that don't affect functionality

### 🔄 Next Steps (Priority Order)

1. **Fix Agent Generic Parameters**
   ```rust
   pub struct ScalingAgent<C: CapsuleCatalog> {
       mesh_node: Arc<MeshNode<C>>,
       compiler: PolicyCompiler,
   }
   ```

2. **Update Test Code**
   - Provide mock `CapsuleCatalog` implementation for tests
   - Update `MeshNode::new()` test calls with required parameters

3. **Complete CapsuleRegistry Integration**
   - Ensure `CapsuleRegistry` implements `CapsuleCatalog` trait
   - Verify `lookup_content()` and `register_content()` work correctly
   - Handle internal mutability for content registration

4. **Add Comprehensive Tests**
   - Unit tests for `ReplicationFrame` serialization (✅ basic tests exist)
   - Integration test with mock TCP connection
   - End-to-end test with real CapsuleRegistry + NvramLog + KeyManager
   - Deduplication test (verify refcount increment)
   - MAC validation failure test
   - Decryption failure test

5. **Update Documentation**
   - [README.md](../../README.md): Add "Inbound Replication" section with flow diagram
   - [CHANGELOG.md](../../CHANGELOG.md): Add entry for fix
   - Create [docs/replication.md](../replication.md) with:
     - Mermaid flow diagram
     - Security guarantees
     - Protocol specification
     - Multi-node setup instructions

## Architecture Design

### Security Flow
```
TCP Stream → Read Frame → Validate MAC → Decrypt → Hash → Dedup Check → Persist → Register
                 ↓            ↓           ↓        ↓          ↓            ↓
              [4B len]    [BLAKE3 MAC] [XTS-256] [BLAKE3] [Registry]  [NvramLog]
```

### Key Components

- **Wire Protocol**: Length-prefixed bincode frames (max 16MB)
- **Integrity**: BLAKE3 MAC validation before processing
- **Confidentiality**: XTS-AES-256 with deterministic tweaks
- **Deduplication**: Post-decryption BLAKE3 content hashing
- **Persistence**: fsync'd NVRAM log writes
- **Metadata**: Content hash → Segment ID mapping in CapsuleCatalog

### Dependencies (No Circular Dependencies!)
```
scaling → common (✅)
scaling → encryption (✅)
scaling → nvram-sim (✅)
capsule-registry → scaling (only with "podms" feature ✅)
```

## Testing Strategy

1. **Unit Tests**: Frame serialization, MAC validation, deduplication logic
2. **Integration Tests**: Mock TCP connections, full handler flow
3. **Multi-Node Tests**: Docker Compose with 3 nodes, verify replication
4. **Fuzz Testing**: proptest for segment data robustness
5. **Benchmarks**: criterion for throughput (target: 1000 segments/sec)

## Timeline Estimate

- Fix compilation errors: 1 hour
- Add comprehensive tests: 2 hours
- Update documentation: 2 hours
- Multi-node testing: 2 hours
- **Total: ~7 hours** (1 day of work)

## Files Modified

1. `crates/scaling/Cargo.toml` - Dependencies
2. `crates/scaling/src/lib.rs` - MeshNode integration
3. `crates/scaling/src/replication.rs` - **NEW**: Handler implementation
4. `crates/scaling/src/agent.rs` - **NEEDS UPDATE**: Generic parameters
5. `README.md` - **NEEDS UPDATE**: Documentation
6. `CHANGELOG.md` - **NEEDS UPDATE**: Changelog entry
7. `docs/replication.md` - **TO CREATE**: Detailed docs

## Notes

- No gossip dependency yet (planned for Step 3)
- Manual peer registration for Step 2 POC
- Telemetry emission is TODO (commented in code)
- RDMA mocked with TCP (production would use rdma-rs)
- Simulation mode compatible (via SPACE_SIM_MODE)

---

**Last Updated**: 2025-11-16
**Implementation Spec**: See project root for full spec document
