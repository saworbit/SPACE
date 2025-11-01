//! Block protocol façade – exposes capsule-backed logical volumes.
//!
//! The contract mimics a very small subset of what an NVMe/NBD target would need:
//! create logical volumes, read ranges, and write ranges.  Each write produces a
//! brand-new capsule so that the immutable data-plane invariants still hold.
//! We eagerly delete superseded capsules via the [`WritePipeline`] helper so that
//! deduplicated segments are correctly reference-counted.

use anyhow::{anyhow, bail, Result};
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::CapsuleId;
use nvram_sim::NvramLog;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_BLOCK_SIZE: u64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockVolume {
    name: String,
    size: u64,
    block_size: u64,
    capsule_id: CapsuleId,
    created_at: u64,
    updated_at: u64,
    version: u64,
}

impl BlockVolume {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

pub struct BlockView {
    pipeline: Arc<WritePipeline>,
    volumes: Arc<RwLock<BTreeMap<String, BlockVolume>>>,
    metadata_path: Option<PathBuf>,
}

impl BlockView {
    /// Construct a new block protocol view.
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        Self {
            pipeline: Arc::new(WritePipeline::new(registry, nvram)),
            volumes: Arc::new(RwLock::new(BTreeMap::new())),
            metadata_path: None,
        }
    }

    /// Open a view backed by an on-disk metadata file.
    pub fn open<P: AsRef<Path>>(
        registry: CapsuleRegistry,
        nvram: NvramLog,
        metadata_path: P,
    ) -> Result<Self> {
        let pipeline = Arc::new(WritePipeline::new(registry, nvram));
        let path = metadata_path.as_ref();
        let volumes = if path.exists() {
            let data = fs::read_to_string(path)?;
            serde_json::from_str(&data)?
        } else {
            BTreeMap::new()
        };

        Ok(Self {
            pipeline,
            volumes: Arc::new(RwLock::new(volumes)),
            metadata_path: Some(path.to_path_buf()),
        })
    }

    fn persist(&self) -> Result<()> {
        if let Some(path) = &self.metadata_path {
            let volumes = self.volumes.read().unwrap();
            let json = serde_json::to_string_pretty(&*volumes)?;
            fs::write(path, json)?;
        }
        Ok(())
    }

    /// Create a volume using the default block size.
    pub fn create_volume(&self, name: &str, size: u64) -> Result<BlockVolume> {
        self.create_volume_with_block_size(name, size, DEFAULT_BLOCK_SIZE)
    }

    /// Create a new logical volume.
    ///
    /// Trade-off: we eagerly zero-initialise the backing capsule so all reads
    /// return deterministic data.  In a production path we would lazily
    /// materialise blocks or use sparse extents.
    pub fn create_volume_with_block_size(
        &self,
        name: &str,
        size: u64,
        block_size: u64,
    ) -> Result<BlockVolume> {
        validate_volume_name(name)?;
        if size == 0 {
            bail!("Volume size must be > 0");
        }
        if block_size == 0 {
            bail!("Block size must be > 0");
        }
        if size % block_size != 0 {
            bail!("Volume size must be a multiple of block size");
        }
        if size > usize::MAX as u64 {
            bail!("Volume size exceeds addressable memory for initialisation");
        }

        {
            let volumes = self.volumes.read().unwrap();
            if volumes.contains_key(name) {
                bail!("Volume already exists: {}", name);
            }
        }

        let buffer = vec![0u8; size as usize];
        let capsule_id = self.pipeline.write_capsule(&buffer)?;
        let now = unix_timestamp();

        let volume = BlockVolume {
            name: name.to_string(),
            size,
            block_size,
            capsule_id,
            created_at: now,
            updated_at: now,
            version: 1,
        };

        let mut volumes = self.volumes.write().unwrap();
        if volumes.contains_key(name) {
            // Another thread raced us – drop the new capsule to avoid leakage.
            drop(volumes);
            let _ = self.pipeline.delete_capsule(capsule_id);
            bail!("Volume already exists: {}", name);
        }
        volumes.insert(name.to_string(), volume.clone());
        drop(volumes);
        self.persist()?;
        Ok(volume)
    }

    /// Return a snapshot of the volume metadata.
    pub fn volume(&self, name: &str) -> Result<BlockVolume> {
        let volumes = self.volumes.read().unwrap();
        volumes
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("Volume not found: {}", name))
    }

    /// List all known volumes (sorted by name because we use `BTreeMap`).
    pub fn list_volumes(&self) -> Vec<BlockVolume> {
        self.volumes.read().unwrap().values().cloned().collect()
    }

    /// Delete a volume and reclaim the underlying capsule.
    pub fn delete_volume(&self, name: &str) -> Result<()> {
        let capsule_id;
        {
            let mut volumes = self.volumes.write().unwrap();
            let volume = volumes
                .remove(name)
                .ok_or_else(|| anyhow!("Volume not found: {}", name))?;
            capsule_id = volume.capsule_id;
        }

        self.persist()?;
        let _ = self.pipeline.delete_capsule(capsule_id);
        Ok(())
    }

    /// Read a byte range from the logical volume.
    pub fn read(&self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let volume = self.volume(name)?;
        if offset + len as u64 > volume.size {
            bail!("Read beyond end of volume");
        }
        self.pipeline.read_range(volume.capsule_id, offset, len)
    }

    /// Overwrite a range within the logical volume.
    ///
    /// We rewrite the whole backing capsule.  The code performs a basic
    /// optimistic concurrency check by verifying that metadata wasn't updated
    /// while we were copying.
    pub fn write(&self, name: &str, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let (capsule_id, version) = {
            let volumes = self.volumes.read().unwrap();
            let volume = volumes
                .get(name)
                .ok_or_else(|| anyhow!("Volume not found: {}", name))?;
            if offset + data.len() as u64 > volume.size {
                bail!("Write beyond end of volume");
            }
            (volume.capsule_id, volume.version)
        };

        let mut buffer = self.pipeline.read_capsule(capsule_id)?;
        let start = offset as usize;
        let end = start + data.len();
        buffer[start..end].copy_from_slice(data);

        let new_capsule = self.pipeline.write_capsule(&buffer)?;
        let now = unix_timestamp();

        let mut volumes = self.volumes.write().unwrap();
        let volume = volumes
            .get_mut(name)
            .ok_or_else(|| anyhow!("Volume not found: {}", name))?;

        if volume.version != version || volume.capsule_id != capsule_id {
            // Somebody mutated the volume while we were rewriting; drop the new capsule
            // and ask the caller to retry.
            drop(volumes);
            let _ = self.pipeline.delete_capsule(new_capsule);
            bail!("Volume modified concurrently");
        }

        volume.capsule_id = new_capsule;
        volume.updated_at = now;
        volume.version = volume.version.saturating_add(1);

        drop(volumes);
        self.persist()?;
        let _ = self.pipeline.delete_capsule(capsule_id);
        Ok(())
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn validate_volume_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Volume name cannot be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("Volume name must be alphanumeric with '-', '_' or '.'");
    }
    Ok(())
}
