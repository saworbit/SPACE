//! The storage-root layout of on-disk format 1 (`docs/on-disk-format.md`
//! §10), shared by [`crate::TokioFsBackend`] and the io_uring backend:
//!
//! - `root/store` holds a `FILE` record of kind 4 with the store id.
//! - `root/segments/<id>.seg` holds one framed `SEGMENT` record: the
//!   segment's write-once descriptor and its stored bytes, with checksums
//!   over both.
//!
//! Each file is written as `<name>.tmp`, fsynced and renamed into place, so a
//! segment file is whole or absent. Leftover `.tmp` files are from writes
//! that never finished, and opening the root deletes them.
//!
//! Ref counts aren't stored. They're held in memory and set from the capsules
//! that reference each segment when a pipeline opens the root
//! ([`FsRoot::recount_references`]), as for the data log.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use common::format::{self, FileHeader, FileKind, Frame, SegmentDescriptor, FRAME_LEN};
use common::{Segment, SegmentId};
use nvram_sim::record_file::sync_parent_dir;
use nvram_sim::RecountReport;

const STORE_FILE: &str = "store";
const SEGMENTS_DIR: &str = "segments";

/// In-memory ref counts. Until [`FsRoot::recount_references`] runs, a segment
/// with no count has 1, as a scanned data log segment does; after it, 0.
#[derive(Default)]
struct Refs {
    counts: HashMap<SegmentId, u32>,
    known: bool,
}

/// An open storage root.
pub(crate) struct FsRoot {
    root: PathBuf,
    header: FileHeader,
    refs: Mutex<Refs>,
}

impl FsRoot {
    /// Open the storage root at `root`, creating it if it's new. A root in
    /// the layout from before format 1 is refused.
    pub(crate) fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .with_context(|| format!("create storage root {}", root.display()))?;
        let store = root.join(STORE_FILE);
        let segments = root.join(SEGMENTS_DIR);
        let header = if store.exists() {
            read_store_file(&store)?
        } else {
            refuse_unknown_contents(root, &segments)?;
            fs::create_dir_all(&segments)?;
            let header = FileHeader::new(FileKind::StorageRoot, now_secs());
            write_file(&store, &header.encode_record())?;
            header
        };
        fs::create_dir_all(&segments)?;
        remove_temp_files(&segments);
        Ok(Self {
            root: root.to_path_buf(),
            header,
            refs: Mutex::new(Refs::default()),
        })
    }

    pub(crate) fn store_id(&self) -> uuid::Uuid {
        self.header.store_id
    }

    pub(crate) fn segment_path(&self, segment: SegmentId) -> PathBuf {
        self.root
            .join(SEGMENTS_DIR)
            .join(format!("{}.seg", segment.0))
    }

    /// The whole file for `segment`: its record, written with `metadata`'s
    /// descriptor, or as plain stored bytes with no content hash if there's
    /// none.
    pub(crate) fn encode_segment(
        segment: SegmentId,
        data: &[u8],
        metadata: Option<&Segment>,
    ) -> Result<Vec<u8>> {
        let described = match metadata {
            Some(m) => Segment {
                id: segment,
                ..m.clone()
            },
            None => Segment {
                id: segment,
                compression_algo: "none".to_string(),
                ..Segment::default()
            },
        };
        let descriptor = SegmentDescriptor::from_segment(&described, data.len())?;
        let mut out = descriptor.encode_head(data)?;
        out.extend_from_slice(data);
        Ok(out)
    }

    /// Write `segment`'s file and set its ref count: `metadata`'s, or 1.
    pub(crate) fn write_segment(
        &self,
        segment: SegmentId,
        data: &[u8],
        metadata: Option<&Segment>,
    ) -> Result<()> {
        let bytes = Self::encode_segment(segment, data, metadata)?;
        write_file(&self.segment_path(segment), &bytes)?;
        self.set_ref_count(segment, metadata.map_or(1, |m| m.ref_count));
        Ok(())
    }

    /// Record that `segment`, written by someone else, is now on disk with
    /// `metadata`'s ref count, or 1.
    #[cfg_attr(not(all(target_os = "linux", feature = "uring")), allow(dead_code))]
    pub(crate) fn wrote_segment(&self, segment: SegmentId, metadata: Option<&Segment>) {
        sync_parent_dir(&self.segment_path(segment));
        self.set_ref_count(segment, metadata.map_or(1, |m| m.ref_count));
    }

    /// Apply metadata for a segment already on disk. Nothing is written: the
    /// descriptor is the one the segment was written with, so only the ref
    /// count changes, as in the data log.
    pub(crate) fn update_metadata(&self, segment: SegmentId, metadata: &Segment) -> Result<()> {
        if !self.segment_path(segment).exists() {
            bail!("Segment not found: {segment:?}");
        }
        self.set_ref_count(segment, metadata.ref_count);
        Ok(())
    }

    /// Commit a transaction's staged writes, metadata and deletes.
    pub(crate) fn commit(
        &self,
        segments: HashMap<SegmentId, Vec<u8>>,
        mut metadata: HashMap<SegmentId, Segment>,
        deleted: Vec<SegmentId>,
    ) -> Result<()> {
        for (segment, data) in segments {
            let described = metadata.remove(&segment);
            self.write_segment(segment, &data, described.as_ref())?;
        }
        for (segment, m) in metadata {
            self.update_metadata(segment, &m)?;
        }
        for segment in deleted {
            self.delete(segment)?;
        }
        Ok(())
    }

    /// Check that `head`, the start of `segment`'s file, which is
    /// `file_len` bytes long, holds the frame and header of a whole record of
    /// that segment, with a matching head checksum. The body isn't summed
    /// here: reads check it by MAC or content hash, as for the data log.
    fn check_head(
        segment: SegmentId,
        head: &[u8],
        file_len: u64,
    ) -> Result<(Frame, SegmentDescriptor)> {
        let too_short = || anyhow!("segment {} file is too short for a record", segment.0);
        let frame_bytes: &[u8; FRAME_LEN] = head
            .get(..FRAME_LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(too_short)?;
        let frame =
            Frame::parse(frame_bytes).with_context(|| format!("segment {} file", segment.0))?;
        if frame.kind != format::kind::SEGMENT || frame.version != format::SEGMENT_VERSION {
            bail!(
                "segment {} file holds a record of kind {} version {}, not a SEGMENT",
                segment.0,
                frame.kind,
                frame.version
            );
        }
        if frame.record_len() != file_len {
            bail!(
                "segment {} file is {} bytes, but its record is {}",
                segment.0,
                file_len,
                frame.record_len()
            );
        }
        let header = head
            .get(FRAME_LEN..frame.head_len() as usize)
            .ok_or_else(too_short)?;
        frame
            .verify_head(header)
            .with_context(|| format!("segment {} file", segment.0))?;
        let descriptor = SegmentDescriptor::decode(header)?;
        if descriptor.segment_id != segment {
            bail!(
                "segment {} file holds segment {}",
                segment.0,
                descriptor.segment_id.0
            );
        }
        Ok((frame, descriptor))
    }

    /// [`FsRoot::check_head`] for a whole file.
    fn parse(segment: SegmentId, file: &[u8]) -> Result<(Frame, SegmentDescriptor)> {
        Self::check_head(segment, file, file.len() as u64)
    }

    fn to_segment(&self, frame: &Frame, descriptor: &SegmentDescriptor) -> Segment {
        let mut segment = descriptor.to_segment(frame.head_len(), frame.body_len);
        segment.ref_count = self.ref_count(descriptor.segment_id);
        segment.deduplicated = segment.ref_count > 1;
        segment
    }

    /// The stored bytes of `segment`, from `file`, its whole file.
    pub(crate) fn body(segment: SegmentId, mut file: Vec<u8>) -> Result<Vec<u8>> {
        let (frame, _) = Self::parse(segment, &file)?;
        Ok(file.split_off(frame.head_len() as usize))
    }

    pub(crate) fn read(&self, segment: SegmentId) -> Result<Vec<u8>> {
        Self::body(segment, self.read_file(segment)?)
    }

    fn read_file(&self, segment: SegmentId) -> Result<Vec<u8>> {
        let path = self.segment_path(segment);
        fs::read(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => anyhow!("Segment not found: {segment:?}"),
            _ => anyhow!(err).context(format!("read {}", path.display())),
        })
    }

    /// `segment`'s metadata, from its record's frame and descriptor, without
    /// reading the stored bytes.
    pub(crate) fn metadata(&self, segment: SegmentId) -> Result<Segment> {
        let path = self.segment_path(segment);
        let mut file = File::open(&path).map_err(|err| match err.kind() {
            ErrorKind::NotFound => anyhow!("Segment not found: {segment:?}"),
            _ => anyhow!(err).context(format!("open {}", path.display())),
        })?;
        let file_len = file.metadata()?.len();
        let mut head = vec![0u8; FRAME_LEN];
        file.read_exact(&mut head)
            .with_context(|| format!("segment {} file is too short for a record", segment.0))?;
        let frame = Frame::parse(head[..].try_into().unwrap())
            .with_context(|| format!("segment {} file", segment.0))?;
        let head_len = frame.head_len().min(file_len) as usize;
        head.resize(head_len.max(FRAME_LEN), 0);
        file.read_exact(&mut head[FRAME_LEN..])?;
        let (frame, descriptor) = Self::check_head(segment, &head, file_len)?;
        Ok(self.to_segment(&frame, &descriptor))
    }

    /// Check `segment`'s file against its record's own checksums: the frame
    /// and descriptor against `head_sum`, the stored bytes against
    /// `body_sum`. Ok(false) means the file isn't a whole, undamaged record
    /// of the segment.
    pub(crate) fn verify(&self, segment: SegmentId) -> Result<bool> {
        let file = self.read_file(segment)?;
        let Ok((frame, _)) = Self::parse(segment, &file) else {
            return Ok(false);
        };
        Ok(frame
            .verify_body(&file[frame.head_len() as usize..])
            .is_ok())
    }

    pub(crate) fn delete(&self, segment: SegmentId) -> Result<()> {
        match fs::remove_file(self.segment_path(segment)) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        self.refs.lock().unwrap().counts.remove(&segment);
        Ok(())
    }

    /// Every segment with a file, in no particular order.
    pub(crate) fn segment_ids(&self) -> Result<Vec<SegmentId>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join(SEGMENTS_DIR))? {
            if let Some(id) = segment_file_id(&entry?.file_name()) {
                out.push(SegmentId(id));
            }
        }
        Ok(out)
    }

    /// One past the highest segment id with a file, so the allocator never
    /// hands out an id that would overwrite one, even after `space.db` is
    /// lost.
    pub(crate) fn segment_id_floor(&self) -> Result<u64> {
        Ok(self
            .segment_ids()?
            .iter()
            .map(|id| id.0.saturating_add(1))
            .max()
            .unwrap_or(0))
    }

    fn ref_count(&self, segment: SegmentId) -> u32 {
        let refs = self.refs.lock().unwrap();
        refs.counts
            .get(&segment)
            .copied()
            .unwrap_or(if refs.known { 0 } else { 1 })
    }

    fn set_ref_count(&self, segment: SegmentId, count: u32) {
        self.refs.lock().unwrap().counts.insert(segment, count);
    }

    /// Set every segment's ref count from `counts`, the number of times
    /// capsules reference each segment. Segments not in `counts` get 0.
    pub(crate) fn recount_references(
        &self,
        counts: &HashMap<SegmentId, u32>,
    ) -> Result<RecountReport> {
        let on_disk = self.segment_ids()?;
        let mut refs = self.refs.lock().unwrap();
        let mut report = RecountReport::default();
        refs.counts.clear();
        for id in &on_disk {
            let count = counts.get(id).copied().unwrap_or(0);
            refs.counts.insert(*id, count);
            if count == 0 {
                report.orphans.push(*id);
            }
        }
        report.missing = counts
            .keys()
            .filter(|id| !refs.counts.contains_key(id))
            .copied()
            .collect();
        refs.known = true;
        report.orphans.sort();
        report.missing.sort();
        Ok(report)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `<id>.seg` → `id`.
fn segment_file_id(name: &std::ffi::OsStr) -> Option<u64> {
    name.to_str()?.strip_suffix(".seg")?.parse().ok()
}

fn read_store_file(path: &Path) -> Result<FileHeader> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let invalid = |why: &str| {
        anyhow!(
            "{} isn't a storage root's store file: {why}",
            path.display()
        )
    };
    let frame_bytes: &[u8; FRAME_LEN] = bytes
        .get(..FRAME_LEN)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| invalid("too short"))?;
    let frame = Frame::parse(frame_bytes).map_err(|err| invalid(&err.to_string()))?;
    if frame.kind != format::kind::FILE || frame.record_len() != bytes.len() as u64 {
        return Err(invalid("not a single FILE record"));
    }
    let header = &bytes[FRAME_LEN..frame.head_len() as usize];
    frame
        .verify_head(header)
        .map_err(|err| invalid(&err.to_string()))?;
    let header = FileHeader::decode(header).map_err(|err| invalid(&err.to_string()))?;
    if header.file_kind != FileKind::StorageRoot {
        return Err(invalid("its FILE record is for another kind of file"));
    }
    if header.format != format::FORMAT_VERSION {
        bail!(
            "{} is in store format {}; this build reads format {}",
            path.display(),
            header.format,
            format::FORMAT_VERSION
        );
    }
    Ok(header)
}

/// A root without a store file must be new. Refuse one holding segments in
/// the layout from before format 1, or segment files without a store file.
fn refuse_unknown_contents(root: &Path, segments: &Path) -> Result<()> {
    let old_layout = root.join("metadata").exists()
        || dir_has(segments, |name| {
            name.ends_with(".bin") || name.ends_with(".stub.json")
        });
    if old_layout {
        bail!(
            "the storage root at {} holds segments in the layout from before on-disk \
             format 1 (segments/<id>.bin and metadata/<id>.json), which this build \
             can't read; see docs/on-disk-format.md §13",
            root.display()
        );
    }
    if dir_has(segments, |name| name.ends_with(".seg")) {
        bail!(
            "the storage root at {} holds segment files but no store file, so it \
             can't be paired with its capsule journal",
            root.display()
        );
    }
    Ok(())
}

fn dir_has(dir: &Path, matches: impl Fn(&str) -> bool) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_str().is_some_and(&matches))
}

/// Delete files left by writes that never finished.
fn remove_temp_files(segments: &Path) {
    let Ok(entries) = fs::read_dir(segments) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".tmp"))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Write `bytes` to `path` so it's whole or absent: to `<path>.tmp`, fsynced,
/// then renamed over `path`, and the directory fsynced.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    sync_parent_dir(path);
    Ok(())
}

/// Run `f` on `root` on a blocking thread: every root operation does file I/O.
pub(crate) fn on_root<T: Send + 'static>(
    root: &std::sync::Arc<FsRoot>,
    f: impl FnOnce(&FsRoot) -> Result<T> + Send + 'static,
) -> futures::future::BoxFuture<'static, Result<T>> {
    let root = std::sync::Arc::clone(root);
    Box::pin(async move { tokio::task::spawn_blocking(move || f(&root)).await? })
}
