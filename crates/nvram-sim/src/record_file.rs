//! Opening and scanning a file of framed records (`docs/on-disk-format.md`
//! §4, §8.1). Shared by the data log and the capsule journal.
//!
//! A record file starts with a `FILE` record. [`open`] creates one if the
//! file is new, checks it otherwise, then walks every record after it:
//!
//! - A record that fails its checks with nothing valid after it is a torn
//!   tail from an interrupted append. The file is truncated back to it.
//! - Invalid bytes with valid records after them are damage. They're
//!   reported, and the scan carries on from the next valid record.
//!
//! Only the last record's body is checksummed: every earlier record was
//! fsynced before the next was appended, so only the last can be torn.

use anyhow::{anyhow, bail, Result};
use common::format::{
    self, FileHeader, FileKind, Frame, FILE_VERSION, FORMAT_VERSION, FRAME_LEN, MAGIC,
};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

/// A byte range that held no valid record when the file was opened, with
/// valid records after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    pub start: u64,
    pub end: u64,
}

/// A record file after [`open`].
pub struct Opened {
    pub file: File,
    pub header: FileHeader,
    /// Offset of the first byte after the `FILE` record.
    pub data_start: u64,
    /// Where the next record goes.
    pub end: u64,
    pub damage: Vec<Damage>,
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Make a newly created file's directory entry durable. Windows has no
/// portable way to fsync a directory, so this is a no-op there.
pub fn sync_parent_dir(path: &Path) {
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

/// Copy the bytes `start..end` of `src` to the current position of `dst`, a
/// buffer at a time. Compaction uses it to move records unchanged.
pub fn copy_range(src: &mut File, dst: &mut File, start: u64, end: u64) -> Result<()> {
    src.seek(SeekFrom::Start(start))?;
    let want = end - start;
    let copied = std::io::copy(&mut Read::by_ref(src).take(want), dst)?;
    if copied != want {
        bail!(
            "the file ended at byte {} while copying bytes {start}..{end}",
            start + copied
        );
    }
    Ok(())
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
        let candidates: Vec<u64> = buf[..want]
            .windows(MAGIC.len())
            .enumerate()
            .filter(|(_, w)| *w == MAGIC)
            .map(|(i, _)| base + i as u64)
            .collect();
        for candidate in candidates {
            if let Probe::Valid(..) = probe(file, candidate, file_len)? {
                return Ok(Some(candidate));
            }
        }
        if want < buf.len() {
            break;
        }
        // Chunks overlap by MAGIC.len() - 1 so a magic split across them is
        // still seen.
        base += CHUNK as u64;
    }
    Ok(None)
}

fn not_format_one(path: &Path, what: &str, why: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "'{}' isn't an on-disk format {FORMAT_VERSION} {what} ({why}). Stores written \
         before format {FORMAT_VERSION} can't be opened; see docs/on-disk-format.md §13.",
        path.display()
    )
}

fn describe(kind: FileKind) -> &'static str {
    match kind {
        FileKind::DataLog => "data log",
        FileKind::Journal => "capsule journal",
        FileKind::Checkpoint => "checkpoint",
        FileKind::StorageRoot => "storage root store file",
    }
}

fn read_file_header(
    file: &mut File,
    path: &Path,
    file_len: u64,
    kind: FileKind,
) -> Result<std::result::Result<(FileHeader, u64), String>> {
    let (frame, header) = match probe(file, 0, file_len)? {
        Probe::Valid(frame, header) => (frame, header),
        Probe::Invalid(reason) => return Ok(Err(reason)),
    };
    let what = describe(kind);
    if frame.kind != format::kind::FILE {
        return Err(not_format_one(
            path,
            what,
            "it doesn't start with a FILE record",
        ));
    }
    if frame.version != FILE_VERSION {
        bail!(
            "'{}' has a FILE record with layout version {}, which this build can't read",
            path.display(),
            frame.version
        );
    }
    let header = FileHeader::decode(&header).map_err(|err| not_format_one(path, what, err))?;
    if header.file_kind != kind {
        bail!(
            "'{}' is a {}, not a {what}",
            path.display(),
            describe(header.file_kind)
        );
    }
    if header.format != FORMAT_VERSION {
        bail!(
            "'{}' is store format {}; this build reads format {FORMAT_VERSION}",
            path.display(),
            header.format
        );
    }
    Ok(Ok((header, frame.record_len())))
}

/// Write a fresh `FILE` record over the whole file, replacing whatever was
/// there, and fsync.
pub fn write_new_file(file: &mut File, path: &Path, header: &FileHeader) -> Result<u64> {
    let record = header.encode_record();
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&record)?;
    file.sync_all()?;
    sync_parent_dir(path);
    Ok(record.len() as u64)
}

/// Open the record file at `path`, creating it if needed, and call `visit`
/// with the offset, frame and header of every valid record after the
/// `FILE` record. `visit` decides what to do with kinds it doesn't know;
/// returning an error stops the open.
pub fn open<F>(path: &Path, kind: FileKind, mut visit: F) -> Result<Opened>
where
    F: FnMut(u64, &Frame, &[u8]) -> Result<()>,
{
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let mut file_len = file.metadata()?.len();
    let what = describe(kind);

    let new_record_len = (FRAME_LEN + FileHeader::LEN) as u64;
    let existing = if file_len == 0 {
        Err("empty".to_string())
    } else {
        read_file_header(&mut file, path, file_len, kind)?
    };
    let (header, data_start) = match existing {
        Ok(found) => found,
        Err(reason) => {
            // Either a new file, or one whose creation was cut short before
            // its FILE record was whole. Neither can hold a record, so it
            // gets a fresh FILE record. Anything else is refused.
            let torn_creation = file_len <= new_record_len && {
                let mut prefix = vec![0u8; file_len as usize];
                file.seek(SeekFrom::Start(0))?;
                file.read_exact(&mut prefix)?;
                prefix.iter().all(|b| *b == 0)
                    || MAGIC.starts_with(&prefix[..prefix.len().min(MAGIC.len())])
            };
            if !torn_creation {
                return Err(not_format_one(path, what, reason));
            }
            if file_len > 0 {
                warn!(
                    path = %path.display(),
                    bytes = file_len,
                    "replacing a FILE record cut short when the file was created"
                );
            }
            let header = FileHeader::new(kind, now_secs());
            file_len = write_new_file(&mut file, path, &header)?;
            (header, file_len)
        }
    };

    let mut damage = Vec::new();
    let mut pos = data_start;
    let mut torn_at = None;
    while pos < file_len {
        let reason = match probe(&mut file, pos, file_len)? {
            Probe::Valid(frame, header) => {
                let end = pos + frame.record_len();
                if frame.kind == format::kind::FILE {
                    bail!(
                        "'{}' has a second FILE record at offset {pos}",
                        path.display()
                    );
                }
                let body_ok = if end == file_len && frame.body_len > 0 {
                    let mut body = vec![0u8; frame.body_len as usize];
                    file.seek(SeekFrom::Start(pos + frame.head_len()))?;
                    file.read_exact(&mut body)?;
                    frame.verify_body(&body).map_err(|err| err.to_string())
                } else {
                    Ok(())
                };
                match body_ok {
                    Ok(()) => {
                        visit(pos, &frame, &header)
                            .map_err(|err| anyhow!("'{}': {err:#}", path.display()))?;
                        pos = end;
                        continue;
                    }
                    Err(reason) => reason,
                }
            }
            Probe::Invalid(reason) => reason,
        };
        match find_next_record(&mut file, pos + 1, file_len)? {
            None => {
                torn_at = Some((pos, reason));
                break;
            }
            Some(next) => {
                warn!(
                    path = %path.display(),
                    start = pos,
                    end = next,
                    reason = reason.as_str(),
                    "record file is damaged"
                );
                damage.push(Damage {
                    start: pos,
                    end: next,
                });
                pos = next;
            }
        }
    }

    if let Some((cut, reason)) = torn_at {
        warn!(
            path = %path.display(),
            offset = cut,
            bytes = file_len - cut,
            reason = reason.as_str(),
            "dropping a record cut short at the end of the file"
        );
        file.set_len(cut)?;
        file.sync_all()?;
        pos = cut;
    }

    Ok(Opened {
        file,
        header,
        data_start,
        end: pos,
        damage,
    })
}

/// Call `visit` with every record between `from` and `to` in a file that
/// [`open`] has already checked.
pub fn read_records<F>(file: &mut File, from: u64, to: u64, mut visit: F) -> Result<()>
where
    F: FnMut(u64, &Frame, &[u8]) -> Result<()>,
{
    let mut pos = from;
    while pos < to {
        match probe(file, pos, to)? {
            Probe::Valid(frame, header) => {
                visit(pos, &frame, &header)?;
                pos += frame.record_len();
            }
            Probe::Invalid(reason) => bail!("record at offset {pos} is invalid: {reason}"),
        }
    }
    Ok(())
}
