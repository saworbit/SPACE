use anyhow::{anyhow, bail, Result};
use common::*;
use serde_json;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};

pub struct NvramLog {
    file: Arc<RwLock<File>>,
    segment_map: Arc<RwLock<HashMap<SegmentId, Segment>>>,
    next_offset: Arc<RwLock<u64>>,
    metadata_path: String,
}

impl NvramLog {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let metadata_path = format!("{}.segments", path_str);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path_str)?;

        // Get file size for next_offset
        let file_len = file.metadata()?.len();

        // Load segment map if exists
        let segment_map = if Path::new(&metadata_path).exists() {
            let data = std::fs::read_to_string(&metadata_path)?;
            serde_json::from_str(&data)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            file: Arc::new(RwLock::new(file)),
            segment_map: Arc::new(RwLock::new(segment_map)),
            next_offset: Arc::new(RwLock::new(file_len)),
            metadata_path,
        })
    }

    fn save_segment_map(&self) -> Result<()> {
        let map = self.segment_map.read().unwrap();
        let json = serde_json::to_string_pretty(&*map)?;
        std::fs::write(&self.metadata_path, json)?;
        Ok(())
    }

    /// List all known segments with their metadata.
    pub fn list_segments(&self) -> Result<Vec<Segment>> {
        Ok(self.segment_map.read().unwrap().values().cloned().collect())
    }

    pub fn append(&self, seg_id: SegmentId, data: &[u8]) -> Result<Segment> {
        let mut file = self.file.write().unwrap();
        let mut next_offset = self.next_offset.write().unwrap();

        let offset = *next_offset;

        // Write to end of file
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.sync_data()?; // fsync for durability

        let segment = Segment {
            id: seg_id,
            offset,
            len: data.len() as u32,
            // Phase 2.1: Compression fields
            compressed: false,
            compression_algo: "none".to_string(),
            // Phase 2.2: Dedup fields
            content_hash: None,
            ref_count: 1, // Default to 1 reference
            deduplicated: false,
            access_count: 0,
            // Phase 3: Encryption fields
            encryption_version: None,
            key_version: None,
            tweak_nonce: None,
            integrity_tag: None,
            encrypted: false,
        };

        *next_offset += data.len() as u64;

        // Update segment map
        self.segment_map
            .write()
            .unwrap()
            .insert(seg_id, segment.clone());

        drop(file);
        drop(next_offset);
        self.save_segment_map()?; // Persist segment map

        Ok(segment)
    }

    /// Increment the refcount for an existing segment.
    pub fn increment_refcount(&self, seg_id: SegmentId) -> Result<Segment> {
        let mut map = self.segment_map.write().unwrap();
        let segment = map
            .get_mut(&seg_id)
            .ok_or_else(|| anyhow!("Segment not found: {:?}", seg_id))?;

        segment.ref_count = segment.ref_count.saturating_add(1);
        segment.deduplicated = segment.ref_count > 1;
        segment.access_count = segment.access_count.saturating_add(1);

        let updated = segment.clone();
        drop(map);
        self.save_segment_map()?;
        Ok(updated)
    }

    /// Decrement the refcount for a segment.
    ///
    /// Returns the updated segment metadata.
    pub fn decrement_refcount(&self, seg_id: SegmentId) -> Result<Segment> {
        let mut map = self.segment_map.write().unwrap();
        let segment = map
            .get_mut(&seg_id)
            .ok_or_else(|| anyhow!("Segment not found: {:?}", seg_id))?;

        if segment.ref_count == 0 {
            bail!("Segment {:?} already has ref_count=0", seg_id);
        }

        segment.ref_count -= 1;
        segment.deduplicated = segment.ref_count > 1;

        let updated = segment.clone();
        drop(map);
        self.save_segment_map()?;
        Ok(updated)
    }

    /// Remove a segment from the metadata map entirely.
    pub fn remove_segment(&self, seg_id: SegmentId) -> Result<Option<Segment>> {
        let mut map = self.segment_map.write().unwrap();
        let removed = map.remove(&seg_id);
        drop(map);
        if removed.is_some() {
            self.save_segment_map()?;
        }
        Ok(removed)
    }

    pub fn read(&self, seg_id: SegmentId) -> Result<Vec<u8>> {
        let segment = self
            .segment_map
            .read()
            .unwrap()
            .get(&seg_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Segment not found"))?;

        let mut file = self.file.write().unwrap();
        file.seek(SeekFrom::Start(segment.offset))?;

        let mut buffer = vec![0u8; segment.len as usize];
        file.read_exact(&mut buffer)?;

        Ok(buffer)
    }

    /// NEW: Get segment metadata without reading data
    ///
    /// Used by the read pipeline to check encryption status and get
    /// encryption metadata before decrypting.
    pub fn get_segment_metadata(&self, seg_id: SegmentId) -> Result<Segment> {
        self.segment_map
            .read()
            .unwrap()
            .get(&seg_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Segment not found: {:?}", seg_id))
    }

    /// NEW: Update segment metadata after encryption
    ///
    /// Called by the write pipeline to update encryption fields after
    /// the segment has been written to disk.
    pub fn update_segment_metadata(&self, seg_id: SegmentId, segment: Segment) -> Result<()> {
        self.segment_map.write().unwrap().insert(seg_id, segment);
        self.save_segment_map()?;
        Ok(())
    }
}

impl Clone for NvramLog {
    fn clone(&self) -> Self {
        Self {
            file: Arc::clone(&self.file),
            segment_map: Arc::clone(&self.segment_map),
            next_offset: Arc::clone(&self.next_offset),
            metadata_path: self.metadata_path.clone(),
        }
    }
}
