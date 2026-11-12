//! The data log: an append-only file of framed `SEGMENT` records
//! (`docs/on-disk-format.md`).
//!
//! The file starts with a `FILE` record, then holds one record per segment:
//! a 48-byte frame, the segment's write-once descriptor, and its stored
//! bytes. Nothing else describes the data, so opening the log scans it and
//! rebuilds the segment map; there is no sidecar to lose (#96).
//!
//! Ref counts aren't stored. They live in memory, start at 1 for every
//! scanned segment, and are set from the registry's capsules with
//! [`NvramLog::recount_references`] when a pipeline opens the log.

use anyhow::{anyhow, bail, Result};

use common::format::{
    self, FileHeader, FileKind, Frame, SegmentDescriptor, FILE_VERSION, FORMAT_VERSION, FRAME_LEN,
    MAGIC, SEGMENT_VERSION,
};
use common::*;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

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

/// A byte range that held no valid record when the log was opened, with
/// valid records after it. A log with damage opens read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    pub start: u64,
    pub end: u64,
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

struct Shared {
    file: RwLock<File>,
    segment_map: RwLock<HashMap<SegmentId, Entry>>,
    next_offset: RwLock<u64>,
    /// Whether every segment's ref count reflects the capsules that hold it.
    /// True for a log that was empty when opened, or once references have
    /// been recounted.
    refs_known: AtomicBool,
}

#[derive(Clone)]
pub struct NvramLog {
    shared: Arc<Shared>,
    path: PathBuf,
    header: FileHeader,
    /// Offset of the first byte after the `FILE` record.
    data_start: u64,
    damage: Arc<Vec<Damage>>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn compacting_marker(path: &Path) -> PathBuf {
    sibling(path, ".compacting")
}

/// Make a newly created file's directory entry durable. Windows has no
/// portable way to fsync a directory, so this is a no-op there.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    {
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The outcome of reading a record's frame and header at some offset.
enum Probe {
    Valid(Frame, Vec<u8>),
    Invalid(String),
}

fn probe(file: &mut File, pos: u64, file_len: u64) -> std::io::Result<Probe> {
    if file_len.saturating_sub(pos) < FRAME_LEN as u64 {
        return Ok(Probe::Invalid("frame cut short".into()));
    }
    let mut frame_bytes = [0u8; FRAME_LEN];
    file.seek(SeekFrom::Start(pos))?;
    file.read_exact(&mut frame_bytes)?;
    let frame = match Frame::parse(&frame_bytes) {
        Ok(frame) => frame,
        Err(err) => return Ok(Probe::Invalid(err.to_string())),
    };
    if pos + frame.record_len() > file_len {
        return Ok(Probe::Invalid(
            "record runs past the end of the file".into(),
        ));
    }
    let mut header = vec![0u8; frame.header_len as usize];
    file.read_exact(&mut header)?;
    if let Err(err) = frame.verify_head(&header) {
        return Ok(Probe::Invalid(err.to_string()));
    }
    Ok(Probe::Valid(frame, header))
}

/// Offset of the first valid record at or after `from`, if any.
fn find_next_record(file: &mut File, from: u64, file_len: u64) -> std::io::Result<Option<u64>> {
    const CHUNK: usize = 1 << 20;
    let mut base = from;
    let mut buf = vec![0u8; CHUNK + MAGIC.len() - 1];
    while base < file_len {
        let want = ((file_len - base) as usize).min(buf.len());
        file.seek(SeekFrom::Start(base))?;
        file.read_exact(&mut buf[..want])?;
        let window = &buf[..want];
        let mut candidates = Vec::new();
        for (i, w) in window.windows(MAGIC.len()).enumerate() {
            if w == MAGIC {
                candidates.push(base + i as u64);
            }
        }
        for candidate in candidates {
            if let Probe::Valid(..) = probe(file, candidate, file_len)? {
                return Ok(Some(candidate));
            }
        }
        if want < buf.len() {
            break;
        }
        // Overlap by MAGIC.len() - 1 so a magic split across chunks is seen.
        base += CHUNK as u64;
    }
    Ok(None)
}

struct Scan {
    entries: HashMap<SegmentId, Entry>,
    /// Where the next record goes.
    end: u64,
    /// Offset and reason of a record cut short at the end of the file.
    torn_tail: Option<(u64, String)>,
    damage: Vec<Damage>,
}

/// Turn a record with a valid head into a map entry. Returns `Ok(Err(..))`
/// when the record is the last in the file and its body fails its checksum:
/// an append cut short, not damage.
fn check_record(
    file: &mut File,
    pos: u64,
    frame: &Frame,
    header: &[u8],
    is_last: bool,
) -> Result<std::result::Result<Option<Entry>, String>> {
    match frame.kind {
        format::kind::SEGMENT => {
            if frame.version != SEGMENT_VERSION {
                bail!(
                    "SEGMENT record at offset {pos} has layout version {}, which this build \
                     can't read",
                    frame.version
                );
            }
            let desc = SegmentDescriptor::decode(header)
                .map_err(|err| anyhow!("SEGMENT record at offset {pos}: {err}"))?;
            let body_offset = pos + frame.head_len();
            // A torn write can only be the last record: every earlier one
            // was fsynced before the next was appended. Earlier bodies are
            // verified by reads (MAC or content hash) and by scrub.
            if is_last {
                let mut body = vec![0u8; frame.body_len as usize];
                file.seek(SeekFrom::Start(body_offset))?;
                file.read_exact(&mut body)?;
                if let Err(err) = frame.verify_body(&body) {
                    return Ok(Err(err.to_string()));
                }
            }
            Ok(Ok(Some(Entry {
                segment: desc.to_segment(body_offset, frame.body_len),
                record_offset: pos,
            })))
        }
        format::kind::FILE => bail!("unexpected FILE record at offset {pos}"),
        _ if frame.is_ignorable() => Ok(Ok(None)),
        other => bail!(
            "record at offset {pos} has kind {other:#04x}, which this build can't read and \
             mustn't skip"
        ),
    }
}

fn scan(file: &mut File, start: u64, file_len: u64) -> Result<Scan> {
    let mut entries: HashMap<SegmentId, Entry> = HashMap::new();
    let mut damage = Vec::new();
    let mut pos = start;
    while pos < file_len {
        let reason = match probe(file, pos, file_len)? {
            Probe::Valid(frame, header) => {
                let end = pos + frame.record_len();
                match check_record(file, pos, &frame, &header, end == file_len)? {
                    Ok(entry) => {
                        if let Some(entry) = entry {
                            let id = entry.segment.id;
                            if entries.insert(id, entry).is_some() {
                                warn!(
                                    segment = id.0,
                                    offset = pos,
                                    "segment id appears twice in the data log; keeping the later record"
                                );
                            }
                        }
                        pos = end;
                        continue;
                    }
                    Err(reason) => reason,
                }
            }
            Probe::Invalid(reason) => reason,
        };
        // `pos` doesn't start a valid record. If one follows, the bytes in
        // between are damage; if none does, this is a torn tail.
        match find_next_record(file, pos + 1, file_len)? {
            None => {
                return Ok(Scan {
                    entries,
                    end: pos,
                    torn_tail: Some((pos, reason)),
                    damage,
                })
            }
            Some(next) => {
                warn!(
                    start = pos,
                    end = next,
                    reason = reason.as_str(),
                    "data log is damaged"
                );
                damage.push(Damage {
                    start: pos,
                    end: next,
                });
                pos = next;
            }
        }
    }
    Ok(Scan {
        entries,
        end: pos,
        torn_tail: None,
        damage,
    })
}

fn not_format_one(path: &Path, why: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "'{}' isn't an on-disk format {FORMAT_VERSION} data log ({why}). Stores written \
         before format {FORMAT_VERSION} can't be opened; see docs/on-disk-format.md §13.",
        path.display()
    )
}

fn read_file_header(file: &mut File, path: &Path, file_len: u64) -> Result<(FileHeader, u64)> {
    let (frame, header) = match probe(file, 0, file_len)? {
        Probe::Valid(frame, header) => (frame, header),
        Probe::Invalid(reason) => return Err(not_format_one(path, reason)),
    };
    if frame.kind != format::kind::FILE {
        return Err(not_format_one(path, "it doesn't start with a FILE record"));
    }
    if frame.version != FILE_VERSION {
        bail!(
            "'{}' has a FILE record with layout version {}, which this build can't read",
            path.display(),
            frame.version
        );
    }
    let header = FileHeader::decode(&header).map_err(|err| not_format_one(path, err))?;
    if header.file_kind != FileKind::DataLog {
        bail!(
            "'{}' is a {:?} file, not a data log",
            path.display(),
            header.file_kind
        );
    }
    if header.format != FORMAT_VERSION {
        bail!(
            "'{}' is store format {}; this build reads format {FORMAT_VERSION}",
            path.display(),
            header.format
        );
    }
    Ok((header, frame.record_len()))
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

        // The in-place compaction rewrites the log front to back. A crash
        // part way leaves it inconsistent, so refuse rather than serve
        // wrong data (#108).
        let marker = compacting_marker(&path);
        if marker.exists() {
            bail!(
                "NvramLog at '{}' has an unfinished compaction marker ('{}' exists). The log \
                 may be inconsistent after a crash during compact(). Remove the marker file \
                 after verifying integrity or restoring from backup.",
                path.display(),
                marker.display()
            );
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let mut file_len = file.metadata()?.len();

        let new_record_len = (FRAME_LEN + FileHeader::LEN) as u64;
        let (header, data_start) = if file_len < new_record_len {
            // Either a new log, or one whose creation was cut short before
            // its FILE record was whole. Neither can hold a segment.
            let mut prefix = vec![0u8; file_len as usize];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut prefix)?;
            let torn_creation = prefix.iter().all(|b| *b == 0)
                || MAGIC.starts_with(&prefix[..prefix.len().min(MAGIC.len())]);
            if !torn_creation {
                return Err(not_format_one(
                    &path,
                    "it's too short to hold a FILE record",
                ));
            }
            if file_len > 0 {
                warn!(
                    path = %path.display(),
                    bytes = file_len,
                    "replacing a FILE record cut short when the data log was created"
                );
            }
            let header = FileHeader::new(FileKind::DataLog, now_secs());
            let record = header.encode_record();
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&record)?;
            file.sync_all()?;
            sync_parent_dir(&path);
            file_len = record.len() as u64;
            (header, file_len)
        } else {
            read_file_header(&mut file, &path, file_len)?
        };

        let scan = scan(&mut file, data_start, file_len)?;
        if let Some((cut, reason)) = &scan.torn_tail {
            warn!(
                path = %path.display(),
                offset = cut,
                bytes = file_len - cut,
                reason = reason.as_str(),
                "dropping a record cut short at the end of the data log"
            );
            file.set_len(*cut)?;
            file.sync_all()?;
        }

        let refs_known = scan.entries.is_empty();
        Ok(Self {
            shared: Arc::new(Shared {
                file: RwLock::new(file),
                segment_map: RwLock::new(scan.entries),
                next_offset: RwLock::new(scan.end),
                refs_known: AtomicBool::new(refs_known),
            }),
            path,
            header,
            data_start,
            damage: Arc::new(scan.damage),
        })
    }

    /// The log's `FILE` record.
    pub fn file_header(&self) -> &FileHeader {
        &self.header
    }

    /// The id of the store this log belongs to.
    pub fn store_id(&self) -> uuid::Uuid {
        self.header.store_id
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
        drop(next_offset);
        drop(file);

        let mut map = self.shared.segment_map.write().unwrap();
        for entry in &placed {
            map.insert(entry.segment.id, entry.clone());
        }
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
        let segment = self
            .shared
            .segment_map
            .read()
            .unwrap()
            .get(&seg_id)
            .map(|e| e.segment.clone())
            .ok_or_else(|| anyhow!("Segment not found"))?;

        let mut file = self.shared.file.write().unwrap();
        file.seek(SeekFrom::Start(segment.offset))?;

        let mut buffer = vec![0u8; segment.len as usize];
        file.read_exact(&mut buffer)?;

        Ok(buffer)
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

    /// Compact the log by rewriting only live segments (`ref_count > 0`).
    ///
    /// Records of segments with `ref_count == 0`, and of segments removed
    /// from the map, are dropped; their bytes are reclaimed. Live records
    /// are copied unchanged to just after the `FILE` record, then the file
    /// is truncated.
    ///
    /// Returns the number of bytes reclaimed.
    ///
    /// ## Crash safety
    ///
    /// A `.compacting` marker file is written before any modification and
    /// removed once the rewrite is fsynced. If the process crashes in
    /// between, `open()` detects the marker and returns an error rather than
    /// serving inconsistent data (#108).
    pub fn compact(&self) -> Result<u64> {
        self.ensure_writable()?;
        if !self.shared.refs_known.load(Ordering::SeqCst) {
            bail!(
                "ref counts haven't been recounted since the data log was opened, so \
                 compaction can't tell live segments from dead ones; call \
                 recount_references first"
            );
        }
        let marker = compacting_marker(&self.path);
        // Plant the marker before touching any file.
        std::fs::write(&marker, b"")?;

        let result = self.do_compact();

        if result.is_ok() {
            let _ = std::fs::remove_file(&marker);
        }
        result
    }

    fn do_compact(&self) -> Result<u64> {
        let mut file = self.shared.file.write().unwrap();
        let mut next_offset = self.shared.next_offset.write().unwrap();
        let mut segment_map = self.shared.segment_map.write().unwrap();

        let old_size = *next_offset;
        if old_size <= self.data_start {
            return Ok(0);
        }

        // Collect live records, sorted by offset for sequential reads.
        let mut live: Vec<Entry> = segment_map
            .values()
            .filter(|e| e.segment.ref_count > 0)
            .cloned()
            .collect();
        live.sort_by_key(|e| e.record_offset);

        // Read all live records while holding the write lock.
        let mut records = Vec::with_capacity(live.len());
        for entry in &live {
            let mut buf = vec![0u8; (entry.record_end() - entry.record_offset) as usize];
            file.seek(SeekFrom::Start(entry.record_offset))?;
            file.read_exact(&mut buf)?;
            records.push(buf);
        }

        // Records hold no offsets, so they move unchanged.
        file.seek(SeekFrom::Start(self.data_start))?;
        let mut pos = self.data_start;
        let mut new_map = HashMap::with_capacity(live.len());
        for (mut entry, bytes) in live.into_iter().zip(records) {
            file.write_all(&bytes)?;
            let head_len = entry.segment.offset - entry.record_offset;
            entry.record_offset = pos;
            entry.segment.offset = pos + head_len;
            pos += bytes.len() as u64;
            new_map.insert(entry.segment.id, entry);
        }

        // Truncate the reclaimed tail and flush.
        file.set_len(pos)?;
        file.sync_data()?;

        *segment_map = new_map;
        *next_offset = pos;

        Ok(old_size.saturating_sub(pos))
    }

    pub fn begin_transaction(&self) -> Result<NvramTransaction> {
        Ok(NvramTransaction::new(self.clone()))
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
    use std::sync::atomic::AtomicUsize;

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
        }
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
