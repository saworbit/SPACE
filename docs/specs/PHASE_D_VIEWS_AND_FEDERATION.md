# SPACE Architecture & Design Specification
# Phase D: Protocol Views & Mesh Federation

**Target Components:** `crates/protocol-*`, `crates/scaling`, `crates/spacectl`  
**Status:** Approved for Implementation  
**Prerequisites:** Phase A-C (Transport Layer) complete  
**Related Documents:** `docs/phase4.md`

## 1. Executive Summary
Goal: Realize the "One Capsule, Infinite Views" thesis. Implement the logic to project a single Data Capsule into multiple standard protocols (NVMe-oF, NFS, FUSE, CSI) simultaneously without data duplication. Activate the Federation Layer so metadata can be sharded across zones via Raft consensus to support those views globally.

Scope:
- **Protocol Adapters:** Implement glue in `crates/protocol-{nvme,nfs,fuse,csi}` to map Capsule I/O to vendor crate APIs.
- **Federation Logic:** Replace `ScalingAction::Federate` no-ops with real `RaftCluster` interactions.
- **Orchestration:** Implement the `spacectl project` command to trigger these views.

## 2. Architecture: The View Projection
Data is not copied to create a view. A Virtual Device translates protocol-specific read/write operations into Capsule segment lookups.

```
graph TD
    User[Client] -- "NVMe / NFS / FUSE" --> Adapter[Protocol Adapter]
    Adapter -- "Read(Offset, Len)" --> VFS[Virtual FS Layer]
    VFS -- "Map to Segments" --> Catalog[Capsule Catalog]
    Catalog -- "Get Data" --> NVRAM[NvramLog]
    
    subgraph "Federation (Sidecar)"
        Adapter -- "Telemetry::ViewProjection" --> Agent[Scaling Agent]
        Agent -- "ScalingAction::Federate" --> Raft[Raft Cluster]
        Raft -- "Shard Metadata" --> Peers[Zone Peers]
    end
```

## 3. Workstream 1: Protocol Adapters
### 3.1 NVMe-oF View (`crates/protocol-nvme`)
- Dependency: `vendor/spdk-rs`
- Logic: Map a Capsule to an SPDK bdev (Block Device), create an NVMf subsystem, emit `Telemetry::ViewProjection` for federation.

```
pub async fn project(capsule: &Capsule, policy: &Policy) -> Result<NvmeView> {
    let bdev_name = format!("capsule-{}", capsule.id.as_uuid());
    spdk_rs::Bdev::register(&bdev_name, capsule.size, |offset, len| {
        // Translate LBA -> SegmentId
    })?;

    let nqn = format!("nqn.2025-11.io.space:{}", capsule.id.as_uuid());
    spdk_rs::NvmfSubsystem::create(&nqn, &bdev_name)?;

    emit_telemetry(Telemetry::ViewProjection { id: capsule.id, view: "nvme".into() }).await;
    Ok(NvmeView { subsystem_nqn: nqn, capsule_id: capsule.id })
}
```

### 3.2 NFS v4.2 View (`crates/protocol-nfs`)
- Dependency: `vendor/nfs-rs`
- Logic: Map a Capsule to a file handle and serve it.

```
pub async fn export_view(capsule: &Capsule, port: u16) -> Result<NfsServer> {
    let fs = VirtualCapsuleFs::new(capsule.clone());
    let server = nfs_rs::Server::new(fs);
    server.listen(format!("0.0.0.0:{port}")).await?;
    Ok(server)
}
```

### 3.3 CSI Driver (`crates/protocol-csi`)
- Dependency: `vendor/csi-driver-rs`
- Logic: Implement the Kubernetes CSI Identity and Node services, mounting the FUSE view for NodePublish.

```
impl CsiNode for SpaceCsiDriver {
    async fn node_publish_volume(&self, req: PublishRequest) -> Result<()> {
        let capsule_id = parse_volume_id(&req.volume_id)?;
        protocol_fuse::mount(capsule_id, &req.target_path).await?;
        Ok(())
    }
}
```

## 4. Workstream 2: Federation & Metadata Sharding
Implement logic for `ScalingAction::Federate` and `ScalingAction::ShardEC` to make view projections durable in the target zones.

```
async fn execute_federation(&self, capsule_id: CapsuleId, zone: ZoneId) -> Result<()> {
    let cluster = RaftCluster::for_zone(&zone.to_string());
    let capsule = self.catalog.lookup_capsule(capsule_id)?;
    let metadata_bytes = serde_json::to_vec(&capsule)?;
    cluster.replicate(&capsule_id.as_uuid().to_string(), &metadata_bytes).await?;
    Ok(())
}

async fn execute_sharding(&self, capsule_id: CapsuleId, zones: Vec<ZoneId>) -> Result<()> {
    for (idx, zone) in zones.iter().enumerate() {
        let shard_key = ShardKey::new(capsule_id.shard_keys(zones.len())[idx]);
        // Store shard in the Raft cluster for that zone
    }
    Ok(())
}
```

## 5. Workstream 3: CLI (`crates/spacectl`)
Implement the `project` subcommand to drive the new APIs.

```
spacectl project --view nvme --id <uuid> --policy-file examples/phase4-policy.yaml
```

The command loads a YAML policy, looks up the capsule, invokes the correct protocol adapter, and stays alive (waiting for `CTRL+C`) to serve traffic.

## 6. Testing Strategy
- **Unit Tests**
  - NVMe: `project()` returns a valid NQN and registers a bdev in the mock SPDK environment.
  - Federation: Mock `RaftCluster` and verify `execute_federation` calls `replicate`.
- **Integration:** `scripts/test_phase4.sh` exercises the end-to-end flow.

```
#!/bin/bash
set -e
./target/release/spacectl daemon &
PID=$!
CAP_ID=$(./target/release/spacectl write --data "Phase4 Data")
./target/release/spacectl project --view nvme --id $CAP_ID --policy-file examples/phase4-policy.yaml &
PROJECT_PID=$!
./scripts/nvmeof_discover.sh | grep $CAP_ID
kill $PROJECT_PID $PID
```

## 7. Migration Guide
- Enable the `phase4` feature in `Cargo.toml`.
- Build with `cargo build --features phase4`.
- Update deployment manifests to include the CSI driver binary when running in Kubernetes.

## 8. Next Steps
- Implement `crates/protocol-nvme/src/lib.rs` (view logic).
- Implement `crates/spacectl/src/main.rs` (CLI projection).
- Un-stub `ScalingAgent` federation logic.
- Verify via `scripts/test_phase4.sh`.
