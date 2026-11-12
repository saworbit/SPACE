//! The data log: an append-only file of framed `SEGMENT` records
//! (`docs/on-disk-format.md`).
//!
//! The file starts with a `FILE` record, then holds one record per segment:
//! a 48-byte frame, the segment's write-once descriptor, and its stored
//! bytes. Nothing else describes the data, so opening the log scans it and
//! rebuilds the segment map; there is no sidecar to lose (#96).
//!
//! Ref counts aren't stored. They live in memory, start at 1 for every
//! scanned segment, and are set from the registry with
//! [`NvramLog::recount_references`] when a pipeline opens the log.
//!
//! Compaction writes the live records to a new generation of the file and
//! renames it over the old one ([`NvramLog::compact`]).

pub mod record_file;

use anyhow::{anyhow, bail, Result};

use common::format::{
    self, FileHeader, FileKind, SegmentDescriptor, FORMAT_VERSION, SEGMENT_VERSION,
};
use common::*;
pub use record_file::Damage;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, TryLockError};
use tracing::{info, warn};

/// A segment in the log: its in-memory metadata (`offset` and `len` locate
/// the body) and where its record starts.
#[derive(Debug, Clone)]
struct Entry {
    segment: Segment,
    record_offset: u64,
}

impl Entry {
    fn record_end(&self) -> u64 {
        self.segment.offset + u64::from(self.segment.len)
    }
}

/// What [`NvramLog::recount_references`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecountReport {
    /// Segments in the log that no capsule references. Compaction reclaims
    /// them.
    pub orphans: Vec<SegmentId>,
    /// Segments some capsule references that aren't in the log.
    pub missing: Vec<SegmentId>,
}

/// Locks are taken in the order `file`, `next_offset`, `segment_map`. An
/// append holds `file` until its records are in the map, so whoever holds
/// `file` sees a map that matches `next_offset`.
struct Shared {
    file: RwLock<File>,
    segment_map: RwLock<HashMap<SegmentId, Entry>>,
    next_offset: RwLock<u64>,
    /// The `FILE` record. Compaction replaces it with the next generation's.
    header: RwLock<FileHeader>,
    /// One past the highest segment id the log has held, counting segments
    /// compaction has since removed.
    id_floor: AtomicU64,
    /// Whether every segment's ref count reflects the capsules that hold it.
    /// True for a log that was empty when opened, or once references have
    /// been recounted.
    refs_known: AtomicBool,
    /// Held while a compaction runs, so only one runs at a time.
    compacting: Mutex<()>,
    /// Called by `read` once it has looked up the segment's offset.
    #[cfg(test)]
    read_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Clone)]
pub struct NvramLog {
    shared: Arc<Shared>,
    path: PathBuf,
    /// Offset of the first byte after the `FILE` record. Every generation's
    /// `FILE` record is the same size, so compaction doesn't change it.
    data_start: u64,
    damage: Arc<Vec<Damage>>,
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Marker left by the in-place compaction of earlier builds.
fn compacting_marker(path: &Path) -> PathBuf {
    sibling(path, ".compacting")
}

/// Where compaction writes the next generation before renaming it into
/// place.
fn compaction_file(path: &Path) -> PathBuf {
    sibling(path, ".compact")
}

/// Points in a compaction where a test can stop it, as a crash there would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The new generation holds the live records; appends since haven't
    /// been copied.
    Copied,
    /// The new generation is complete and fsynced, but not renamed.
    Synced,
    /// The new generation has replaced the log on disk.
    Renamed,
}

/// Copy the bytes `start..end` of `src` to the current position of `dst`, a
/// buffer at a time.
fn copy_range(src: &mut File, dst: &mut File, start: u64, end: u64) -> Result<()> {
    src.seek(SeekFrom::Start(start))?;
    let want = end - start;
    let copied = std::io::copy(&mut Read::by_ref(src).take(want), dst)?;
    if copied != want {
        bail!(
            "the data log ended at byte {} while copying bytes {start}..{end}",
            start + copied
        );
    }
    Ok(())
}

impl NvramLog {
    /// Open the data log at `path`, creating it if it doesn't exist.
    ///
    /// The log is scanned to rebuild the segment map. A record cut short at
    /// the end of the file (an append interrupted by a crash) is truncated
    /// away. Bytes that hold no valid record but are followed by valid ones
    /// are damage: the log then opens read-only and [`NvramLog::damage`]
    /// lists the ranges.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        for suffix in [".segments", ".segments.tmp", ".segments.compacting"] {
            let legacy = sibling(&path, suffix);
            if legacy.exists() {
                bail!(
                    "'{}' belongs to a store written before on-disk format {FORMAT_VERSION}, \
                     which this build can't open (docs/on-disk-format.md §13). Read its data \
                     out with the version that wrote it, then move the old files aside.",
                    legacy.display()
                );
            }
        }

        // Earlier builds compacted in place, front to back. A crash part way
        // through left the log inconsistent, so refuse rather than serve
        // wrong data.
        let marker = compacting_marker(&path);
        if marker.exists() {
            bail!(
                "NvramLog at '{}' has a marker from an unfinished in-place compaction ('{}' \
                 exists), left by an earlier build. The log may be inconsistent. Remove the \
                 marker after verifying integrity or restoring from backup.",
                path.display(),
                marker.display()
            );
        }

        // A compaction that didn't get as far as its rename leaves the next
        // generation behind. The log it would have replaced is intact.
        let unfinished = compaction_file(&path);
        if unfinished.exists() {
            warn!(
                path = %unfinished.display(),
                "removing the file of a compaction that didn't finish; the data log is intact"
            );
            std::fs::remove_file(&unfinished)?;
        }

        let mut entries: HashMap<SegmentId, Entry> = HashMap::new();
        let mut id_floor = 0u64;
        let opened = record_file::open(&path, FileKind::DataLog, |pos, frame, header| match frame
            .kind
        {
            format::kind::SEGMENT => {
                if frame.version != SEGMENT_VERSION {
                    bail!(
                        "SEGMENT record at offset {pos} has layout version {}, which this \
                             build can't read",
                        frame.version
                    );
                }
                let desc = SegmentDescriptor::decode(header)
                    .map_err(|err| anyhow!("SEGMENT record at offset {pos}: {err}"))?;
                let entry = Entry {
                    segment: desc.to_segment(pos + frame.head_len(), frame.body_len),
                    record_offset: pos,
                };
                let id = entry.segment.id;
                id_floor = id_floor.max(id.0.saturating_add(1));
                if entries.insert(id, entry).is_some() {
                    warn!(
                        segment = id.0,
                        offset = pos,
                        "segment id appears twice in the data log; keeping the later record"
                    );
                }
                Ok(())
            }
            _ if frame.is_ignorable() => Ok(()),
            other => {
                bail!("record at offset {pos} has kind {other:#04x}, which a data log can't hold")
            }
        })?;

        let refs_known = entries.is_empty();
        let id_floor = id_floor.max(opened.header.segment_id_floor);
        Ok(Self {
            shared: Arc::new(Shared {
                file: RwLock::new(opened.file),
                segment_map: RwLock::new(entries),
                next_offset: RwLock::new(opened.end),
                header: RwLock::new(opened.header),
                id_floor: AtomicU64::new(id_floor),
                refs_known: AtomicBool::new(refs_known),
                compacting: Mutex::new(()),
                #[cfg(test)]
                read_hook: Mutex::new(None),
            }),
            path,
            data_start: opened.data_start,
            damage: Arc::new(opened.damage),
        })
    }

    /// The log's `FILE` record.
    pub fn file_header(&self) -> FileHeader {
        *self.shared.header.read().unwrap()
    }

    /// The id of the store this log belongs to.
    pub fn store_id(&self) -> uuid::Uuid {
        self.file_header().store_id
    }

    /// The lowest segment id the allocator may hand out: one past every id
    /// the log has held, including segments compaction has removed since,
    /// so no id is used twice (`docs/on-disk-format.md` §8.3).
    pub fn segment_id_floor(&self) -> u64 {
        self.shared.id_floor.load(Ordering::SeqCst)
    }

    /// Damaged ranges found when the log was opened. Non-empty means the
    /// log is read-only.
    pub fn damage(&self) -> &[Damage] {
        &self.damage
    }

    fn ensure_writable(&self) -> Result<()> {
        if let Some(first) = self.damage.first() {
            bail!(
                "the data log at '{}' is open read-only: {} damaged range(s), the first at \
                 bytes {}..{}",
                self.path.display(),
                self.damage.len(),
                first.start,
                first.end
            );
        }
        Ok(())
    }

    /// Bytes of segment data the map knows about (sum of `len` fields).
    ///
    /// Deleted segments stay in the file until compaction, and opening the
    /// log maps them again, so this reflects bytes on disk rather than live
    /// data.
    pub fn used_bytes(&self) -> u64 {
        self.shared
            .segment_map
            .read()
            .unwrap()
            .values()
            .map(|e| u64::from(e.segment.len))
            .sum()
    }

    /// Bytes in the log file.
    pub fn size(&self) -> u64 {
        *self.shared.next_offset.read().unwrap()
    }

    /// Bytes compaction would free now: everything in the file but its
    /// `FILE` record and the records of segments with references.
    pub fn reclaimable_bytes(&self) -> u64 {
        let end = *self.shared.next_offset.read().unwrap();
        let map = self.shared.segment_map.read().unwrap();
        let live: u64 = map
            .values()
            .filter(|e| e.segment.ref_count > 0)
            .map(|e| e.record_end() - e.record_offset)
            .sum();
        end.saturating_sub(self.data_start + live)
    }

    /// List all known segments with their metadata.
    pub fn list_segments(&self) -> Result<Vec<Segment>> {
        Ok(self
            .shared
            .segment_map
            .read()
            .unwrap()
            .values()
            .map(|e| e.segment.clone())
            .collect())
    }

    /// Append one record per segment at the end of the log and fsync. Each
    /// `Segment` supplies the descriptor and in-memory state; its offset
    /// and length are set here. If the write fails, the file is cut back so
    /// nothing half-written is left past the end.
    fn append_records(&self, items: &[(Segment, Vec<u8>)]) -> Result<Vec<Segment>> {
        self.ensure_writable()?;
        let mut heads = Vec::with_capacity(items.len());
        for (segment, data) in items {
            let desc = SegmentDescriptor::from_segment(segment, data.len())
                .map_err(|err| anyhow!("segment {:?}: {err}", segment.id))?;
            let head = desc
                .encode_head(data)
                .map_err(|err| anyhow!("segment {:?}: {err}", segment.id))?;
            heads.push((desc, head));
        }

        let mut file = self.shared.file.write().unwrap();
        let mut next_offset = self.shared.next_offset.write().unwrap();
        let base = *next_offset;
        let mut pos = base;
        let mut placed = Vec::with_capacity(items.len());

        let written: Result<()> = (|| {
            file.seek(SeekFrom::Start(base))?;
            for ((segment, data), (desc, head)) in items.iter().zip(&heads) {
                file.write_all(head)?;
                file.write_all(data)?;
                let body_offset = pos + head.len() as u64;
                let len = u32::try_from(data.len())?;
                let mut stored = desc.to_segment(body_offset, len);
                stored.ref_count = segment.ref_count;
                stored.deduplicated = segment.deduplicated;
                stored.access_count = segment.access_count;
                placed.push(Entry {
                    segment: stored,
                    record_offset: pos,
                });
                pos = body_offset + u64::from(len);
            }
            file.sync_data()?;
            Ok(())
        })();
        if let Err(err) = written {
            let _ = file.set_len(base);
            return Err(err);
        }
        *next_offset = pos;

        // Still holding `file`, so a compaction never sees these records
        // below `next_offset` but missing from the map.
        let mut map = self.shared.segment_map.write().unwrap();
        for entry in &placed {
            self.shared
                .id_floor
                .fetch_max(entry.segment.id.0.saturating_add(1), Ordering::SeqCst);
            map.insert(entry.segment.id, entry.clone());
        }
        drop(map);
        drop(next_offset);
        drop(file);
        Ok(placed.into_iter().map(|e| e.segment).collect())
    }

    /// Append a segment with no descriptor beyond its bytes: stored as is,
    /// uncompressed and unencrypted, with no content hash.
    pub fn append(&self, seg_id: SegmentId, data: &[u8]) -> Result<Segment> {
        let mut stored = self.append_records(&[(new_segment(seg_id), data.to_vec())])?;
        Ok(stored.remove(0))
    }

    fn update_in_memory<F>(&self, seg_id: SegmentId, f: F) -> Result<Segment>
    where
        F: FnOnce(&mut Segment) -> Result<()>,
    {
        let mut map = self.shared.segment_map.write().unwrap();
        let entry = map
            .get_mut(&seg_id)
            .ok_or_else(|| anyhow!("Segment not found: {:?}", seg_id))?;
        f(&mut entry.segment)?;
        Ok(entry.segment.clone())
    }

    /// Increment the in-memory refcount for an existing segment.
    pub fn increment_refcount(&self, seg_id: SegmentId) -> Result<Segment> {
        self.update_in_memory(seg_id, |segment| {
            segment.ref_count = segment.ref_count.saturating_add(1);
            segment.deduplicated = segment.ref_count > 1;
            segment.access_count = segment.access_count.saturating_add(1);
            Ok(())
        })
    }

    /// Decrement the in-memory refcount for a segment.
    pub fn decrement_refcount(&self, seg_id: SegmentId) -> Result<Segment> {
        self.update_in_memory(seg_id, |segment| {
            if segment.ref_count == 0 {
                bail!("Segment {:?} already has ref_count=0", seg_id);
            }
            segment.ref_count -= 1;
            segment.deduplicated = segment.ref_count > 1;
            Ok(())
        })
    }

    /// Remove a segment from the map. Its record stays in the file until
    /// compaction, so the next open maps it again, with no references.
    pub fn remove_segment(&self, seg_id: SegmentId) -> Result<Option<Segment>> {
        Ok(self
            .shared
            .segment_map
            .write()
            .unwrap()
            .remove(&seg_id)
            .map(|e| e.segment))
    }

    pub fn read(&self, seg_id: SegmentId) -> Result<Vec<u8>> {
        // The file lock first: compaction swaps the file and the offsets
        // under it, so an offset is only good for the file it was read with.
        let mut file = self.shared.file.write().unwrap();
        let segment = self
            .shared
            .segment_map
            .read()
            .unwrap()
            .get(&seg_id)
            .map(|e| e.segment.clone())
            .ok_or_else(|| anyhow!("Segment not found"))?;
        #[cfg(test)]
        {
            let hook = self.shared.read_hook.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        file.seek(SeekFrom::Start(segment.offset))?;

        let mut buffer = vec![0u8; segment.len as usize];
        file.read_exact(&mut buffer)?;

        Ok(buffer)
    }

    /// Check a segment's record against its own checksums: the frame and
    /// descriptor against `head_sum`, the stored bytes against `body_sum`.
    /// They're unkeyed, so this catches media damage but not tampering, and
    /// needs no key. Ok(false) means a checksum didn't match.
    pub fn verify_record(&self, seg_id: SegmentId) -> Result<bool> {
        let mut file = self.shared.file.write().unwrap();
        let entry = self
            .shared
            .segment_map
            .read()
            .unwrap()
            .get(&seg_id)
            .cloned()
            .ok_or_else(|| anyhow!("Segment not found: {:?}", seg_id))?;
        let mut record = vec![0u8; (entry.record_end() - entry.record_offset) as usize];
        file.seek(SeekFrom::Start(entry.record_offset))?;
        file.read_exact(&mut record)?;
        drop(file);

        let Some(frame_bytes) = record.get(..format::FRAME_LEN) else {
            return Ok(false);
        };
        let Ok(frame) = format::Frame::parse(frame_bytes.try_into().unwrap()) else {
            return Ok(false);
        };
        if frame.record_len() != record.len() as u64 {
            return Ok(false);
        }
        let head_end = frame.head_len() as usize;
        Ok(frame
            .verify_head(&record[format::FRAME_LEN..head_end])
            .is_ok()
            && frame.verify_body(&record[head_end..]).is_ok())
    }

    /// Get segment metadata without reading data.
    pub fn get_segment_metadata(&self, seg_id: SegmentId) -> Result<Segment> {
        self.shared
            .segment_map
            .read()
            .unwrap()
            .get(&seg_id)
            .map(|e| e.segment.clone())
            .ok_or_else(|| anyhow!("Segment not found: {:?}", seg_id))
    }

    /// Replace an existing segment's in-memory metadata, keeping its offset
    /// and length.
    ///
    /// Nothing is written: the descriptor on disk is the one the segment was
    /// written with, and ref counts are recounted when the log is opened.
    /// Only ref-count changes are meant to come through here.
    pub fn update_segment_metadata(&self, seg_id: SegmentId, segment: Segment) -> Result<()> {
        self.update_in_memory(seg_id, |current| {
            let mut merged = segment;
            merged.id = seg_id;
            merged.offset = current.offset;
            merged.len = current.len;
            *current = merged;
            Ok(())
        })?;
        Ok(())
    }

    /// Set every segment's ref count from `counts`, the number of times
    /// capsules reference each segment. Segments not in `counts` get 0.
    ///
    /// Ref counts aren't stored in the log, so this must run before the log
    /// serves writes or deletes. Until it does, scanned segments carry a
    /// ref count of 1 and compaction refuses to run.
    pub fn recount_references(&self, counts: &HashMap<SegmentId, u32>) -> RecountReport {
        let mut map = self.shared.segment_map.write().unwrap();
        let mut report = RecountReport::default();
        for (id, entry) in map.iter_mut() {
            let count = counts.get(id).copied().unwrap_or(0);
            entry.segment.ref_count = count;
            entry.segment.deduplicated = count > 1;
            if count == 0 {
                report.orphans.push(*id);
            }
        }
        report.missing = counts
            .keys()
            .filter(|id| !map.contains_key(id))
            .copied()
            .collect();
        report.orphans.sort();
        report.missing.sort();
        self.shared.refs_known.store(true, Ordering::SeqCst);
        report
    }

    /// Reclaim the bytes of segments nothing references, by writing the
    /// live records to the next generation of the log and renaming it over
    /// this one (`docs/on-disk-format.md` §9.1). Returns the bytes reclaimed.
    ///
    /// A segment is live while its ref count is above 0. That includes
    /// segments an open write holds, and those of a dropped capsule a reader
    /// still holds.
    ///
    /// 1. Note the live records and where the log ends.
    /// 2. Without holding the log's locks, write `<log>.compact`: a `FILE`
    ///    record for the next generation, then each live record copied
    ///    unchanged, a buffer at a time. Records hold no offsets, so their
    ///    checksums stay valid. Reads and appends carry on meanwhile.
    /// 3. Under the log's locks, copy the records appended since step 1, and
    ///    any segment that became live, fsync, and rename the new file over
    ///    the log.
    ///
    /// A crash before the rename leaves `<log>.compact`, which the next open
    /// removes; the old log is intact. A crash after it leaves the complete
    /// new generation. Either opens without manual steps (#108).
    pub fn compact(&self) -> Result<u64> {
        self.compact_with(|_| false)
    }

    /// [`NvramLog::compact`], calling `stop_at` at each [`Step`]. When it
    /// returns true, compaction stops there and leaves the files as a crash
    /// would. Only tests stop it; the in-memory state is then stale, so the
    /// log must be dropped and opened again.
    fn compact_with(&self, mut stop_at: impl FnMut(Step) -> bool) -> Result<u64> {
        self.ensure_writable()?;
        if !self.shared.refs_known.load(Ordering::SeqCst) {
            bail!(
                "ref counts haven't been recounted since the data log was opened, so \
                 compaction can't tell live segments from dead ones; call \
                 recount_references first"
            );
        }
        let _running = match self.shared.compacting.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                bail!(
                    "a compaction of '{}' is already running",
                    self.path.display()
                )
            }
        };

        // 1. The live records, and where the log ends. Holding `file` keeps
        // appends out, so the map matches `next_offset`.
        let (live, snapshot_end, header) = {
            let _file = self.shared.file.write().unwrap();
            let end = *self.shared.next_offset.read().unwrap();
            let map = self.shared.segment_map.read().unwrap();
            let mut live: Vec<Entry> = map
                .values()
                .filter(|e| e.segment.ref_count > 0)
                .cloned()
                .collect();
            live.sort_by_key(|e| e.record_offset);
            (live, end, *self.shared.header.read().unwrap())
        };
        let live_bytes: u64 = live.iter().map(|e| e.record_end() - e.record_offset).sum();
        if self.data_start + live_bytes == snapshot_end {
            return Ok(0);
        }

        // 2. Copy them to the next generation.
        let next_path = compaction_file(&self.path);
        let mut next = FileHeader {
            generation: header.generation + 1,
            segment_id_floor: self.segment_id_floor(),
            ..header
        };
        let mut out = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&next_path)?;
        let copied = (|| -> Result<(u64, HashMap<SegmentId, u64>)> {
            let mut pos = record_file::write_new_file(&mut out, &next_path, &next)?;
            // A handle of its own, so reads and appends needn't wait. It's
            // closed before the rename below.
            let mut src = File::open(&self.path)?;
            let mut moved = HashMap::with_capacity(live.len());
            for entry in &live {
                copy_range(&mut src, &mut out, entry.record_offset, entry.record_end())?;
                moved.insert(entry.segment.id, pos);
                pos += entry.record_end() - entry.record_offset;
            }
            Ok((pos, moved))
        })();
        let (mut pos, mut moved) = match copied {
            Ok(copied) => copied,
            Err(err) => {
                drop(out);
                let _ = std::fs::remove_file(&next_path);
                return Err(err);
            }
        };
        if stop_at(Step::Copied) {
            bail!("compaction stopped at {:?}", Step::Copied);
        }

        // 3. Catch up and swap, with reads and appends held off.
        let mut file = self.shared.file.write().unwrap();
        let mut next_offset = self.shared.next_offset.write().unwrap();
        let mut map = self.shared.segment_map.write().unwrap();
        let old_end = *next_offset;
        let tail_base = pos;
        let caught_up = (|| -> Result<u64> {
            // Records appended since step 1 sit together at the end.
            copy_range(&mut file, &mut out, snapshot_end, old_end)?;
            let mut pos = tail_base + (old_end - snapshot_end);
            // A segment with no references can't gain one, since nothing
            // can find it to dedup against. Copy any that did anyway.
            let mut revived: Vec<&Entry> = map
                .values()
                .filter(|e| {
                    e.segment.ref_count > 0
                        && e.record_offset < snapshot_end
                        && !moved.contains_key(&e.segment.id)
                })
                .collect();
            revived.sort_by_key(|e| e.record_offset);
            for entry in revived {
                warn!(
                    segment = entry.segment.id.0,
                    "a segment gained references during compaction; copying it"
                );
                copy_range(&mut file, &mut out, entry.record_offset, entry.record_end())?;
                moved.insert(entry.segment.id, pos);
                pos += entry.record_end() - entry.record_offset;
            }
            // Ids appended since step 1 count toward the floor.
            next.segment_id_floor = self.segment_id_floor();
            out.seek(SeekFrom::Start(0))?;
            out.write_all(&next.encode_record())?;
            out.sync_all()?;
            Ok(pos)
        })();
        pos = match caught_up {
            Ok(pos) => pos,
            Err(err) => {
                drop(out);
                let _ = std::fs::remove_file(&next_path);
                return Err(err);
            }
        };
        if stop_at(Step::Synced) {
            bail!("compaction stopped at {:?}", Step::Synced);
        }

        // Windows won't rename over a file that's open, so close the old log
        // first. The new file can be renamed while open.
        let old = std::mem::replace(&mut *file, out);
        drop(old);
        if let Err(err) = std::fs::rename(&next_path, &self.path) {
            let restored = OpenOptions::new().read(true).write(true).open(&self.path);
            let _ = std::fs::remove_file(&next_path);
            return match restored {
                Ok(old) => {
                    *file = old;
                    Err(anyhow!(
                        "couldn't move the compacted log into place: {err}; the log is unchanged"
                    ))
                }
                Err(reopen) => Err(anyhow!(
                    "couldn't move the compacted log into place ({err}) or reopen the old one \
                     ({reopen}); reopen the store"
                )),
            };
        }
        record_file::sync_parent_dir(&self.path);
        if stop_at(Step::Renamed) {
            bail!("compaction stopped at {:?}", Step::Renamed);
        }

        // Point the map at the new offsets. Segments left out were dead.
        let mut new_map = HashMap::with_capacity(moved.len());
        for (id, mut entry) in map.drain() {
            let new_offset = if entry.record_offset >= snapshot_end {
                Some(tail_base + (entry.record_offset - snapshot_end))
            } else {
                moved.get(&id).copied()
            };
            if let Some(record_offset) = new_offset {
                let head_len = entry.segment.offset - entry.record_offset;
                entry.record_offset = record_offset;
                entry.segment.offset = record_offset + head_len;
                new_map.insert(id, entry);
            }
        }
        *map = new_map;
        *next_offset = pos;
        *self.shared.header.write().unwrap() = next;

        let reclaimed = old_end.saturating_sub(pos);
        info!(
            path = %self.path.display(),
            generation = next.generation,
            reclaimed,
            "compacted the data log"
        );
        Ok(reclaimed)
    }

    pub fn begin_transaction(&self) -> Result<NvramTransaction> {
        Ok(NvramTransaction::new(self.clone()))
    }

    /// The highest segment id in the map, if any.
    pub fn max_segment_id(&self) -> Option<SegmentId> {
        self.shared
            .segment_map
            .read()
            .unwrap()
            .keys()
            .max()
            .copied()
    }

    pub fn list_segment_ids(&self) -> Vec<SegmentId> {
        self.shared
            .segment_map
            .read()
            .unwrap()
            .keys()
            .copied()
            .collect()
    }
}

/// In-memory metadata for a segment appended with no descriptor yet.
fn new_segment(seg_id: SegmentId) -> Segment {
    Segment {
        id: seg_id,
        offset: 0,
        len: 0,
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
    }
}

struct PendingSegment {
    segment: Segment,
    data: Vec<u8>,
}

/// Segments appended together. Nothing reaches the file until `commit`,
/// which writes every record, fsyncs once, and assigns offsets.
pub struct NvramTransaction {
    log: NvramLog,
    pending: Vec<PendingSegment>,
    finalized: bool,
}

impl NvramTransaction {
    fn new(log: NvramLog) -> Self {
        Self {
            log,
            pending: Vec::new(),
            finalized: false,
        }
    }

    fn ensure_active(&self) -> Result<()> {
        if self.finalized {
            bail!("transaction already finalized");
        }
        Ok(())
    }

    /// Stage a segment. Its descriptor can be filled in with
    /// `set_segment_metadata` before commit; the returned metadata has no
    /// offset yet.
    pub fn append_segment(&mut self, seg_id: SegmentId, data: &[u8]) -> Result<Segment> {
        self.ensure_active()?;
        let mut segment = new_segment(seg_id);
        segment.len = u32::try_from(data.len())?;
        self.pending.push(PendingSegment {
            segment: segment.clone(),
            data: data.to_vec(),
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

    /// Set a staged segment's descriptor, or an existing segment's
    /// in-memory metadata (ref-count changes from dedup hits and deletes).
    pub fn set_segment_metadata(&mut self, seg_id: SegmentId, segment: Segment) -> Result<()> {
        self.ensure_active()?;

        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|entry| entry.segment.id == seg_id)
        {
            let mut merged = segment;
            merged.id = seg_id;
            merged.len = pending.segment.len;
            pending.segment = merged;
            return Ok(());
        }

        self.log.update_segment_metadata(seg_id, segment)
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
        self.finalized = true;
        if self.pending.is_empty() {
            return Ok(());
        }
        let items: Vec<(Segment, Vec<u8>)> = self
            .pending
            .drain(..)
            .map(|p| (p.segment, p.data))
            .collect();
        self.log.append_records(&items)?;
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        if self.finalized {
            return Ok(());
        }
        self.pending.clear();
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::format::FRAME_LEN;
    use std::sync::atomic::AtomicUsize;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempLog {
        path: PathBuf,
    }

    impl TempLog {
        fn new(name: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join("space_nvram_sim_tests");
            std::fs::create_dir_all(&dir).unwrap();
            let unique = format!(
                "{name}_{}_{}_{}.nvram",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst),
                now_nanos()
            );
            Self {
                path: dir.join(unique),
            }
        }

        fn open(&self) -> NvramLog {
            NvramLog::open(&self.path).unwrap()
        }

        fn len(&self) -> u64 {
            std::fs::metadata(&self.path).unwrap().len()
        }
    }

    impl Drop for TempLog {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(compacting_marker(&self.path));
            let _ = std::fs::remove_file(compaction_file(&self.path));
        }
    }

    /// Segments 1 to 4, of which 1 and 2 are referenced.
    fn half_live(tmp: &TempLog) -> NvramLog {
        let log = tmp.open();
        for i in 1..=4 {
            commit_described(&log, i, &payload(i));
        }
        drop(log);
        let log = tmp.open();
        log.recount_references(&[(SegmentId(1), 1), (SegmentId(2), 1)].into());
        log
    }

    fn sorted_ids(log: &NvramLog) -> Vec<SegmentId> {
        let mut ids = log.list_segment_ids();
        ids.sort();
        ids
    }

    fn now_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn described(id: u64, data: &[u8]) -> Segment {
        Segment {
            id: SegmentId(id),
            plain_len: Some(data.len() as u32 * 2),
            compressed: true,
            compression_algo: "lz4:1".into(),
            content_hash: Some(ContentHash::from_bytes(&[id as u8; 32])),
            ref_count: 1,
            encrypted: true,
            encryption_version: Some(1),
            key_version: Some(3),
            tweak_nonce: Some([9u8; 16]),
            integrity_tag: Some([8u8; 16]),
            ..Segment::default()
        }
    }

    fn commit_described(log: &NvramLog, id: u64, data: &[u8]) {
        let mut txn = log.begin_transaction().unwrap();
        txn.append_segment(SegmentId(id), data).unwrap();
        txn.set_segment_metadata(SegmentId(id), described(id, data))
            .unwrap();
        txn.commit().unwrap();
    }

    fn payload(i: u64) -> Vec<u8> {
        (0..100 + i * 7).map(|b| (b as u8) ^ (i as u8)).collect()
    }

    #[test]
    fn segments_survive_reopen_without_a_sidecar() {
        let tmp = TempLog::new("reopen");
        let log = tmp.open();
        log.append(SegmentId(1), b"raw bytes").unwrap();
        commit_described(&log, 2, b"described bytes");
        let store_id = log.store_id();
        drop(log);

        assert!(!sibling(&tmp.path, ".segments").exists());
        let log = tmp.open();
        assert_eq!(log.store_id(), store_id);
        assert_eq!(log.read(SegmentId(1)).unwrap(), b"raw bytes");
        assert_eq!(log.read(SegmentId(2)).unwrap(), b"described bytes");

        let raw = log.get_segment_metadata(SegmentId(1)).unwrap();
        assert_eq!(raw.plain_len, Some(9));
        assert_eq!(raw.content_hash, None);
        assert!(!raw.encrypted);

        let seg = log.get_segment_metadata(SegmentId(2)).unwrap();
        let want = described(2, b"described bytes");
        assert_eq!(seg.plain_len, want.plain_len);
        assert_eq!(seg.compression_algo, want.compression_algo);
        assert_eq!(seg.content_hash, want.content_hash);
        assert_eq!(seg.key_version, want.key_version);
        assert_eq!(seg.tweak_nonce, want.tweak_nonce);
        assert_eq!(seg.integrity_tag, want.integrity_tag);
        assert!(seg.compressed && seg.encrypted);
        assert_eq!(seg.len, 15);
    }

    #[test]
    fn a_record_cut_short_anywhere_is_dropped_and_the_rest_reads() {
        let tmp = TempLog::new("torn");
        let log = tmp.open();
        for i in 1..=3 {
            commit_described(&log, i, &payload(i));
        }
        drop(log);
        let full = std::fs::read(&tmp.path).unwrap();
        let log = tmp.open();
        let last = log.get_segment_metadata(SegmentId(3)).unwrap();
        let last_start = last.offset - (FRAME_LEN as u64 + 84 + 2 + 5);
        drop(log);

        for cut in last_start..full.len() as u64 {
            std::fs::write(&tmp.path, &full[..cut as usize]).unwrap();
            let log = tmp.open();
            assert!(log.damage().is_empty(), "cut at {cut} reported damage");
            for i in 1..=2 {
                assert_eq!(log.read(SegmentId(i)).unwrap(), payload(i), "cut at {cut}");
            }
            assert!(log.get_segment_metadata(SegmentId(3)).is_err());
            assert_eq!(tmp.len(), last_start, "cut at {cut} wasn't truncated back");
            // The log keeps working after the truncation.
            commit_described(&log, 4, b"after");
            drop(log);
            assert_eq!(tmp.open().read(SegmentId(4)).unwrap(), b"after");
        }
    }

    #[test]
    fn a_rotted_last_body_counts_as_a_torn_tail() {
        let tmp = TempLog::new("tail_body");
        let log = tmp.open();
        commit_described(&log, 1, &payload(1));
        commit_described(&log, 2, &payload(2));
        let end = tmp.len();
        drop(log);

        let mut bytes = std::fs::read(&tmp.path).unwrap();
        bytes[end as usize - 1] ^= 0xFF;
        std::fs::write(&tmp.path, &bytes).unwrap();

        let log = tmp.open();
        assert!(log.damage().is_empty());
        assert!(log.get_segment_metadata(SegmentId(2)).is_err());
        assert_eq!(log.read(SegmentId(1)).unwrap(), payload(1));
    }

    #[test]
    fn damage_in_the_middle_opens_read_only_and_keeps_later_records() {
        let tmp = TempLog::new("damage");
        let log = tmp.open();
        for i in 1..=3 {
            commit_described(&log, i, &payload(i));
        }
        let middle = log.get_segment_metadata(SegmentId(2)).unwrap();
        drop(log);

        let mut bytes = std::fs::read(&tmp.path).unwrap();
        let frame_start = middle.offset as usize - (FRAME_LEN + 84 + 2 + 5);
        bytes[frame_start + 10] ^= 0xFF;
        std::fs::write(&tmp.path, &bytes).unwrap();

        let log = tmp.open();
        assert_eq!(log.damage().len(), 1);
        assert_eq!(log.damage()[0].start, frame_start as u64);
        assert_eq!(log.read(SegmentId(1)).unwrap(), payload(1));
        assert_eq!(log.read(SegmentId(3)).unwrap(), payload(3));
        assert!(log.get_segment_metadata(SegmentId(2)).is_err());
        let err = log.append(SegmentId(9), b"no").unwrap_err();
        assert!(err.to_string().contains("read-only"), "{err}");
        // Nothing was truncated.
        assert_eq!(tmp.len(), bytes.len() as u64);
    }

    #[test]
    fn verify_record_catches_a_flipped_byte_in_any_record() {
        let tmp = TempLog::new("verify");
        let log = tmp.open();
        for i in 1..=3 {
            commit_described(&log, i, &payload(i));
        }
        for i in 1..=3 {
            assert!(log.verify_record(SegmentId(i)).unwrap());
        }
        let middle = log.get_segment_metadata(SegmentId(2)).unwrap();
        drop(log);

        // A body byte of the middle record: open checks only the last body.
        let mut bytes = std::fs::read(&tmp.path).unwrap();
        bytes[middle.offset as usize + 3] ^= 0x10;
        std::fs::write(&tmp.path, &bytes).unwrap();

        let log = tmp.open();
        assert!(log.damage().is_empty());
        assert!(log.verify_record(SegmentId(1)).unwrap());
        assert!(!log.verify_record(SegmentId(2)).unwrap());
        assert!(log.verify_record(SegmentId(3)).unwrap());
        assert!(log.verify_record(SegmentId(9)).is_err());
    }

    #[test]
    fn stores_from_before_format_one_are_refused() {
        let tmp = TempLog::new("legacy");
        std::fs::write(&tmp.path, vec![0x42u8; 4096]).unwrap();
        let err = NvramLog::open(&tmp.path).err().unwrap();
        assert!(err.to_string().contains("format 1"), "{err}");

        let sidecar = sibling(&tmp.path, ".segments");
        std::fs::write(&sidecar, b"{}").unwrap();
        let err = NvramLog::open(&tmp.path).err().unwrap();
        let _ = std::fs::remove_file(&sidecar);
        assert!(err.to_string().contains("before on-disk format"), "{err}");
    }

    #[test]
    fn a_file_record_cut_short_at_creation_is_replaced() {
        let tmp = TempLog::new("creation");
        std::fs::write(&tmp.path, b"SPC").unwrap();
        let log = tmp.open();
        assert_eq!(log.file_header().file_kind, FileKind::DataLog);
        log.append(SegmentId(1), b"ok").unwrap();
        drop(log);
        assert_eq!(tmp.open().read(SegmentId(1)).unwrap(), b"ok");
    }

    #[test]
    fn compaction_waits_for_a_recount_then_drops_unreferenced_records() {
        let tmp = TempLog::new("compact");
        let log = tmp.open();
        for i in 1..=4 {
            commit_described(&log, i, &payload(i));
        }
        drop(log);

        let log = tmp.open();
        assert!(log.compact().is_err());

        let counts: HashMap<SegmentId, u32> =
            [(SegmentId(2), 1), (SegmentId(4), 2), (SegmentId(7), 1)].into();
        let report = log.recount_references(&counts);
        assert_eq!(report.orphans, vec![SegmentId(1), SegmentId(3)]);
        assert_eq!(report.missing, vec![SegmentId(7)]);
        assert!(log.get_segment_metadata(SegmentId(4)).unwrap().deduplicated);

        let before = tmp.len();
        let reclaimed = log.compact().unwrap();
        assert!(reclaimed > 0);
        assert_eq!(tmp.len(), before - reclaimed);
        assert_eq!(log.read(SegmentId(2)).unwrap(), payload(2));
        assert_eq!(log.read(SegmentId(4)).unwrap(), payload(4));
        drop(log);

        let log = tmp.open();
        let mut ids = log.list_segment_ids();
        ids.sort();
        assert_eq!(ids, vec![SegmentId(2), SegmentId(4)]);
        assert_eq!(log.read(SegmentId(4)).unwrap(), payload(4));
    }

    #[test]
    fn removed_segments_come_back_until_compaction() {
        let tmp = TempLog::new("removed");
        let log = tmp.open();
        commit_described(&log, 1, b"gone soon");
        log.remove_segment(SegmentId(1)).unwrap();
        assert!(log.get_segment_metadata(SegmentId(1)).is_err());
        drop(log);

        let log = tmp.open();
        assert!(log.get_segment_metadata(SegmentId(1)).is_ok());
        let report = log.recount_references(&HashMap::new());
        assert_eq!(report.orphans, vec![SegmentId(1)]);
        log.compact().unwrap();
        drop(log);
        assert!(tmp.open().list_segment_ids().is_empty());
    }

    #[test]
    fn a_new_log_needs_no_recount_to_compact() {
        let tmp = TempLog::new("fresh");
        let log = tmp.open();
        commit_described(&log, 1, b"live");
        log.append(SegmentId(2), b"dead").unwrap();
        log.decrement_refcount(SegmentId(2)).unwrap();
        log.compact().unwrap();
        assert_eq!(log.list_segment_ids(), vec![SegmentId(1)]);
    }

    #[test]
    fn compaction_writes_the_next_generation_and_keeps_the_id_floor() {
        let tmp = TempLog::new("generation");
        let log = half_live(&tmp);
        let store_id = log.store_id();
        assert_eq!(log.file_header().generation, 0);

        assert!(log.compact().unwrap() > 0);
        assert_eq!(log.file_header().generation, 1);
        assert_eq!(log.read(SegmentId(2)).unwrap(), payload(2));
        // Appends land after the copied records.
        commit_described(&log, 5, b"after compaction");
        drop(log);

        let log = tmp.open();
        assert!(!compaction_file(&tmp.path).exists());
        assert_eq!(log.store_id(), store_id);
        assert_eq!(log.file_header().generation, 1);
        assert_eq!(sorted_ids(&log), [SegmentId(1), SegmentId(2), SegmentId(5)]);
        assert_eq!(log.read(SegmentId(5)).unwrap(), b"after compaction");
        drop(log);

        // Compacting away the highest ids doesn't lower the floor.
        let log = tmp.open();
        log.recount_references(&[(SegmentId(1), 1)].into());
        log.compact().unwrap();
        drop(log);
        let log = tmp.open();
        assert_eq!(sorted_ids(&log), [SegmentId(1)]);
        assert_eq!(log.file_header().generation, 2);
        assert_eq!(log.segment_id_floor(), 6);
    }

    #[test]
    fn reclaimable_bytes_is_what_compaction_frees() {
        let tmp = TempLog::new("reclaimable");
        let log = half_live(&tmp);
        assert_eq!(log.size(), tmp.len());
        let expected = log.reclaimable_bytes();
        assert!(expected > 0);
        assert_eq!(log.compact().unwrap(), expected);
        assert_eq!(log.reclaimable_bytes(), 0);
        assert_eq!(log.size(), tmp.len());
    }

    #[test]
    fn a_log_with_nothing_to_reclaim_is_left_alone() {
        let tmp = TempLog::new("nothing");
        let log = tmp.open();
        commit_described(&log, 1, b"live");
        assert_eq!(log.reclaimable_bytes(), 0);
        let before = tmp.len();
        assert_eq!(log.compact().unwrap(), 0);
        assert_eq!(log.file_header().generation, 0);
        assert_eq!(tmp.len(), before);
    }

    #[test]
    fn a_crash_at_any_step_leaves_a_log_that_opens() {
        for step in [Step::Copied, Step::Synced, Step::Renamed] {
            let tmp = TempLog::new("crash");
            let log = half_live(&tmp);
            assert!(log.compact_with(|at| at == step).is_err());
            drop(log);

            let log = tmp.open();
            assert!(log.damage().is_empty(), "{step:?}");
            assert!(!compaction_file(&tmp.path).exists(), "{step:?}");
            let (generation, ids) = if step == Step::Renamed {
                (1, vec![SegmentId(1), SegmentId(2)])
            } else {
                (0, (1..=4).map(SegmentId).collect())
            };
            assert_eq!(log.file_header().generation, generation, "{step:?}");
            assert_eq!(sorted_ids(&log), ids, "{step:?}");
            for i in 1..=2 {
                assert_eq!(log.read(SegmentId(i)).unwrap(), payload(i), "{step:?}");
            }

            // And it compacts normally afterwards.
            log.recount_references(&[(SegmentId(1), 1), (SegmentId(2), 1)].into());
            log.compact().unwrap();
            drop(log);
            let log = tmp.open();
            assert_eq!(sorted_ids(&log), [SegmentId(1), SegmentId(2)], "{step:?}");
        }
    }

    #[test]
    fn appends_and_releases_during_the_copy_are_caught_up() {
        let tmp = TempLog::new("concurrent");
        let log = half_live(&tmp);
        log.compact_with(|at| {
            if at == Step::Copied {
                // No lock is held while the live records are copied.
                assert!(log.compact().is_err(), "a second compaction started");
                log.append(SegmentId(10), b"appended mid-copy").unwrap();
                commit_described(&log, 11, &payload(11));
                assert_eq!(log.read(SegmentId(1)).unwrap(), payload(1));
                log.remove_segment(SegmentId(2)).unwrap();
                // Dead when the copy started; the catch-up copies it anyway.
                log.increment_refcount(SegmentId(3)).unwrap();
            }
            false
        })
        .unwrap();

        assert_eq!(
            sorted_ids(&log),
            [SegmentId(1), SegmentId(3), SegmentId(10), SegmentId(11)]
        );
        assert_eq!(log.read(SegmentId(3)).unwrap(), payload(3));
        log.decrement_refcount(SegmentId(3)).unwrap();
        assert_eq!(log.read(SegmentId(10)).unwrap(), b"appended mid-copy");
        assert_eq!(log.read(SegmentId(11)).unwrap(), payload(11));
        assert_eq!(log.segment_id_floor(), 12);
        drop(log);

        // Segment 2 was copied before it was released, so it comes back with
        // no references and goes at the next compaction.
        let log = tmp.open();
        assert_eq!(log.file_header().segment_id_floor, 12);
        assert_eq!(log.read(SegmentId(11)).unwrap(), payload(11));
        let report = log.recount_references(
            &[(SegmentId(1), 1), (SegmentId(10), 1), (SegmentId(11), 1)].into(),
        );
        assert_eq!(report.orphans, [SegmentId(2), SegmentId(3)]);
        log.compact().unwrap();
        assert_eq!(
            sorted_ids(&log),
            [SegmentId(1), SegmentId(10), SegmentId(11)]
        );
    }

    #[test]
    fn a_read_never_uses_an_offset_from_another_generation() {
        use std::sync::mpsc;
        use std::time::Duration;

        // Segment 4 is live behind dead ones, so compaction moves it.
        let tmp = TempLog::new("read_race");
        let log = tmp.open();
        for i in 1..=4 {
            commit_described(&log, i, &payload(i));
        }
        drop(log);
        let log = tmp.open();
        log.recount_references(&[(SegmentId(2), 1), (SegmentId(4), 1)].into());

        // Once the read has looked up the offset, let a whole compaction run
        // before it touches the file, if one can.
        let (looked_up, wait_lookup) = mpsc::channel::<()>();
        let (compacted, wait_compacted) = mpsc::channel::<()>();
        let (looked_up, wait_compacted) = (Mutex::new(looked_up), Mutex::new(wait_compacted));
        *log.shared.read_hook.lock().unwrap() = Some(Arc::new(move || {
            let _ = looked_up.lock().unwrap().send(());
            let _ = wait_compacted
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(300));
        }));

        let reader = {
            let log = log.clone();
            std::thread::spawn(move || log.read(SegmentId(4)))
        };
        wait_lookup.recv().unwrap();
        *log.shared.read_hook.lock().unwrap() = None;
        let (done, wait_done) = mpsc::channel();
        let compactor = {
            let log = log.clone();
            std::thread::spawn(move || done.send(log.compact().map(|_| ())).unwrap())
        };
        // The read holds the file, so the compaction has to wait for it.
        if wait_done.recv_timeout(Duration::from_millis(150)).is_ok() {
            compacted.send(()).unwrap();
        }

        let read = reader.join().unwrap();
        assert!(
            matches!(&read, Ok(bytes) if *bytes == payload(4)),
            "the read used an offset from the old generation: {read:?}"
        );
        compactor.join().unwrap();
        assert_eq!(log.file_header().generation, 1);
        assert_eq!(log.read(SegmentId(4)).unwrap(), payload(4));
    }

    #[test]
    fn a_rolled_back_transaction_writes_nothing() {
        let tmp = TempLog::new("rollback");
        let log = tmp.open();
        let before = tmp.len();
        let mut txn = log.begin_transaction().unwrap();
        txn.append_segment(SegmentId(1), b"never").unwrap();
        txn.rollback().unwrap();
        assert_eq!(tmp.len(), before);
        assert!(log.list_segment_ids().is_empty());
    }
}
