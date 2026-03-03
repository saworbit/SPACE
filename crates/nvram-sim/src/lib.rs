use anyhow::{anyhow, bail, Result};
#[cfg(feature = "advanced-security")]
use common::security::audit_log::AuditLog;
use common::*;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};
#[cfg(feature = "advanced-security")]
use tracing::warn;

pub struct NvramLog {
    file: Arc<RwLock<File>>,
    segment_map: Arc<RwLock<HashMap<SegmentId, Segment>>>,
    next_offset: Arc<RwLock<u64>>,
    metadata_path: String,
    #[cfg(feature = "advanced-security")]
    audit_log: Option<AuditLog>,
}

impl NvramLog {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let metadata_path = format!("{}.segments", path_str);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path_str)?;

        // Get file size for next_offset
        let file_len = file.metadata()?.len();

        // Detect an interrupted compaction.  If we crashed after the data
        // file was rewritten but before the new metadata was committed, the
        // log may be inconsistent.  Fail loudly rather than silently serving
        // wrong data.  The operator can remove the marker after verifying
        // (or restoring from backup) and re-opening.
        let compacting_path = format!("{}.compacting", metadata_path);
        if Path::new(&compacting_path).exists() {
            bail!(
                "NvramLog at '{}' has an unfinished compaction marker \
                 ('{}' exists). The log may be inconsistent after a crash \
                 during compact(). Remove the marker file after verifying \
                 integrity or restoring from backup.",
                path_str,
                compacting_path
            );
        }

        // Load segment map if exists.  Also recover from an orphaned .tmp
        // file — this happens on Windows if we crashed between the
        // remove_file() and rename() calls in save_segment_map().
        let segment_map = {
            let tmp_path = format!("{}.tmp", metadata_path);
            if Path::new(&metadata_path).exists() {
                let data = std::fs::read_to_string(&metadata_path)?;
                serde_json::from_str(&data)?
            } else if Path::new(&tmp_path).exists() {
                let data = std::fs::read_to_string(&tmp_path)?;
                let map: HashMap<SegmentId, Segment> = serde_json::from_str(&data)?;
                // Complete the interrupted rename so the store is consistent.
                let _ = std::fs::rename(&tmp_path, &metadata_path);
                map
            } else {
                HashMap::new()
            }
        };

        Ok(Self {
            file: Arc::new(RwLock::new(file)),
            segment_map: Arc::new(RwLock::new(segment_map)),
            next_offset: Arc::new(RwLock::new(file_len)),
            metadata_path,
            #[cfg(feature = "advanced-security")]
            audit_log: None,
        })
    }

    #[cfg(feature = "advanced-security")]
    pub fn with_audit(mut self, audit_log: AuditLog) -> Self {
        self.audit_log = Some(audit_log);
        self
    }

    fn save_segment_map(&self) -> Result<()> {
        let map = self.segment_map.read().unwrap();
        let json = serde_json::to_string_pretty(&*map)?;
        // Write to a .tmp file first, then atomically rename over the target.
        // On POSIX, rename(2) atomically replaces the destination — the
        // previous good metadata is never overwritten until the new one is
        // fully written to disk.  On Windows, rename fails if the destination
        // already exists, so we remove it first; if we crash in that narrow
        // window the orphaned .tmp is recovered at open() time below.
        let tmp_path = format!("{}.tmp", self.metadata_path);
        std::fs::write(&tmp_path, json.as_bytes())?;
        match std::fs::rename(&tmp_path, &self.metadata_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&self.metadata_path)?;
                std::fs::rename(&tmp_path, &self.metadata_path)?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// Total bytes occupied by live segment data (sum of `len` fields).
    ///
    /// Segments with `ref_count == 0` are included until GC runs — this
    /// reflects physical bytes on the log, not logical live data.
    pub fn used_bytes(&self) -> u64 {
        self.segment_map
            .read()
            .unwrap()
            .values()
            .map(|s| s.len as u64)
            .sum()
    }

    #[cfg(feature = "advanced-security")]
    fn log_segment(&self, segment: &Segment) {
        if let Some(audit) = &self.audit_log {
            let event = Event::SegmentAppended {
                segment_id: segment.id,
                len: segment.len,
                content_hash: segment.content_hash.clone(),
                encrypted: segment.encrypted,
            };
            if let Err(err) = audit.append(event) {
                warn!(error = %err, "failed to append audit log entry");
            }
        }
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
            plain_len: None,
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
            pq_ciphertext: None,
            pq_nonce: None,
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

        #[cfg(feature = "advanced-security")]
        self.log_segment(&updated);

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

    /// Compact the log by rewriting only live segments (`ref_count > 0`).
    ///
    /// Segments with `ref_count == 0` are skipped — their bytes are
    /// permanently reclaimed.  All live segments are rewritten sequentially
    /// from offset 0, then the file is truncated.  The metadata sidecar is
    /// updated atomically afterwards.
    ///
    /// Returns the number of bytes reclaimed.
    ///
    /// ## Crash safety
    ///
    /// A `.compacting` marker file is written before any modification and
    /// removed only after `save_segment_map()` completes.  If the process
    /// crashes between the data rewrite and the metadata commit, `open()`
    /// detects the marker and returns an error rather than serving
    /// inconsistent data.
    pub fn compact(&self) -> Result<u64> {
        let compacting_path = format!("{}.compacting", self.metadata_path);
        // Plant the marker before touching any file.
        std::fs::write(&compacting_path, b"")?;

        let result = self.do_compact();

        if result.is_ok() {
            let _ = std::fs::remove_file(&compacting_path);
        }
        result
    }

    fn do_compact(&self) -> Result<u64> {
        let mut file = self.file.write().unwrap();
        let mut segment_map = self.segment_map.write().unwrap();
        let mut next_offset = self.next_offset.write().unwrap();

        let old_size = *next_offset;
        if old_size == 0 {
            return Ok(0);
        }

        // Collect live segments (sorted by offset for sequential reads).
        let mut live: Vec<(SegmentId, Segment)> = segment_map
            .iter()
            .filter(|(_, s)| s.ref_count > 0)
            .map(|(&id, s)| (id, s.clone()))
            .collect();
        live.sort_by_key(|(_, s)| s.offset);

        // Read all live segment bytes while holding the write lock.
        let mut payloads: Vec<(SegmentId, Segment, Vec<u8>)> = Vec::with_capacity(live.len());
        for (id, seg) in live {
            let mut buf = vec![0u8; seg.len as usize];
            file.seek(SeekFrom::Start(seg.offset))?;
            file.read_exact(&mut buf)?;
            payloads.push((id, seg, buf));
        }

        // Rewrite live segments to the beginning of the same file.
        file.seek(SeekFrom::Start(0))?;
        let mut new_offset = 0u64;
        let mut new_map = HashMap::new();
        for (id, mut seg, data) in payloads {
            file.write_all(&data)?;
            seg.offset = new_offset;
            new_offset += data.len() as u64;
            new_map.insert(id, seg);
        }

        // Truncate the reclaimed tail and flush.
        file.set_len(new_offset)?;
        file.sync_data()?;

        // Update in-memory state while still holding locks.
        *segment_map = new_map;
        *next_offset = new_offset;

        drop(file);
        drop(segment_map);
        drop(next_offset);

        // Commit the updated segment map atomically (tmp+rename).
        self.save_segment_map()?;

        Ok(old_size.saturating_sub(new_offset))
    }

    pub fn begin_transaction(&self) -> Result<NvramTransaction> {
        let base_offset = *self.next_offset.read().unwrap();
        Ok(NvramTransaction::new(self.clone(), base_offset))
    }

    pub fn list_segment_ids(&self) -> Vec<SegmentId> {
        self.segment_map.read().unwrap().keys().copied().collect()
    }
}

impl Clone for NvramLog {
    fn clone(&self) -> Self {
        Self {
            file: Arc::clone(&self.file),
            segment_map: Arc::clone(&self.segment_map),
            next_offset: Arc::clone(&self.next_offset),
            metadata_path: self.metadata_path.clone(),
            #[cfg(feature = "advanced-security")]
            audit_log: self.audit_log.clone(),
        }
    }
}

struct PendingSegment {
    segment: Segment,
    data: Vec<u8>,
}

pub struct NvramTransaction {
    log: NvramLog,
    pending: Vec<PendingSegment>,
    base_offset: u64,
    current_offset: u64,
    finalized: bool,
}

impl NvramTransaction {
    fn new(log: NvramLog, base_offset: u64) -> Self {
        Self {
            log,
            pending: Vec::new(),
            base_offset,
            current_offset: base_offset,
            finalized: false,
        }
    }

    fn ensure_active(&self) -> Result<()> {
        if self.finalized {
            bail!("transaction already finalized");
        }
        Ok(())
    }

    pub fn append_segment(&mut self, seg_id: SegmentId, data: &[u8]) -> Result<Segment> {
        self.ensure_active()?;

        let offset = self.current_offset;
        let data_vec = data.to_vec();
        let len = data_vec.len() as u32;

        let segment = Segment {
            id: seg_id,
            offset,
            len,
            plain_len: None,
            compressed: false,
            compression_algo: "none".to_string(),
            content_hash: None,
            ref_count: 1,
            deduplicated: false,
            access_count: 0,
            encryption_version: None,
            key_version: None,
            tweak_nonce: None,
            integrity_tag: None,
            encrypted: false,
            pq_ciphertext: None,
            pq_nonce: None,
        };

        self.current_offset = offset + data_vec.len() as u64;
        {
            let mut next_offset = self.log.next_offset.write().unwrap();
            *next_offset = self.current_offset;
        }

        self.pending.push(PendingSegment {
            segment: segment.clone(),
            data: data_vec,
        });

        Ok(segment)
    }

    pub fn with_segment_mut<F>(&mut self, seg_id: SegmentId, f: F) -> Result<()>
    where
        F: FnOnce(&mut Segment),
    {
        self.ensure_active()?;

        let pending = self
            .pending
            .iter_mut()
            .find(|entry| entry.segment.id == seg_id)
            .ok_or_else(|| anyhow!("pending segment {:?} not found", seg_id))?;

        f(&mut pending.segment);
        Ok(())
    }

    pub fn set_segment_metadata(&mut self, seg_id: SegmentId, segment: Segment) -> Result<()> {
        self.ensure_active()?;

        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|entry| entry.segment.id == seg_id)
        {
            let mut merged = segment;
            merged.id = seg_id;
            merged.offset = pending.segment.offset;
            merged.len = pending.segment.len;
            pending.segment = merged;
            return Ok(());
        }

        // Metadata update for an existing segment (eg. refcount bumps from dedup hits).
        // Preserve storage-derived offset/len from the committed record.
        let existing = self.log.get_segment_metadata(seg_id)?;
        let mut merged = segment;
        merged.id = seg_id;
        merged.offset = existing.offset;
        merged.len = existing.len;
        self.log.update_segment_metadata(seg_id, merged)?;
        Ok(())
    }

    pub fn pending_segment(&self, seg_id: SegmentId) -> Option<&Segment> {
        self.pending
            .iter()
            .find(|entry| entry.segment.id == seg_id)
            .map(|entry| &entry.segment)
    }

    pub fn log_handle(&self) -> NvramLog {
        self.log.clone()
    }

    pub fn commit(&mut self) -> Result<()> {
        if self.finalized {
            return Ok(());
        }

        if self.pending.is_empty() {
            self.finalized = true;
            return Ok(());
        }

        let mut file = self.log.file.write().unwrap();
        let mut next_offset = self.log.next_offset.write().unwrap();

        let write_result: Result<()> = (|| {
            for entry in &self.pending {
                file.seek(SeekFrom::Start(entry.segment.offset))?;
                file.write_all(&entry.data)?;
            }
            file.sync_data()?;
            Ok(())
        })();

        if let Err(err) = write_result {
            *next_offset = self.base_offset;
            self.finalized = true;
            return Err(err);
        }

        *next_offset = self.current_offset;
        drop(file);
        drop(next_offset);

        {
            let mut map = self.log.segment_map.write().unwrap();
            for entry in &self.pending {
                map.insert(entry.segment.id, entry.segment.clone());
                #[cfg(feature = "advanced-security")]
                self.log.log_segment(&entry.segment);
            }
        }
        self.log.save_segment_map()?;

        self.pending.clear();
        self.finalized = true;
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        if self.finalized {
            return Ok(());
        }

        let mut next_offset = self.log.next_offset.write().unwrap();
        *next_offset = self.base_offset;
        self.pending.clear();
        self.current_offset = self.base_offset;
        self.finalized = true;
        Ok(())
    }
}

impl Drop for NvramTransaction {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = self.rollback();
        }
    }
}
