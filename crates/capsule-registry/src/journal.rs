//! The capsule journal (`docs/on-disk-format.md` §5–§7): an append-only file
//! of `CAPSULE`, `DROP` and `BATCH` records. It's the durable record of which
//! capsules exist; sled is an index rebuilt from it.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use common::format::{FileHeader, FileKind, JournalRecord};
use nvram_sim::record_file;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How far into which journal the index has applied records. Stored in
/// sled next to the index, and updated in the same transaction as each
/// record's effects.
/// Version of the index sled keeps. Bump it whenever the encoding of an index
/// record changes: an index of another version is rebuilt from the journal.
pub const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalPosition {
    pub index_version: u32,
    pub store_id: Uuid,
    pub generation: u64,
    /// Offset just past the last applied record.
    pub offset: u64,
}

struct Inner {
    file: File,
    header: FileHeader,
    data_start: u64,
    end: u64,
}

pub struct Journal {
    inner: Mutex<Inner>,
    path: PathBuf,
}

impl Journal {
    /// The journal that sits next to the sled database at `db_path`.
    pub fn path_for(db_path: &Path) -> PathBuf {
        db_path.with_extension("journal")
    }

    /// Open the journal at `path`, creating it if needed. Every record is
    /// checked; a torn last record is dropped. A journal with damage inside
    /// it is refused, since the records in the damaged range are lost.
    pub fn open(path: &Path) -> Result<Self> {
        let opened = record_file::open(path, FileKind::Journal, |pos, frame, header| {
            JournalRecord::decode(frame, header)
                .map(|_| ())
                .map_err(|err| anyhow!("journal record at offset {pos}: {err}"))
        })?;
        if let Some(first) = opened.damage.first() {
            bail!(
                "the capsule journal '{}' is damaged at bytes {}..{}; the capsules recorded \
                 there can't be recovered from it",
                path.display(),
                first.start,
                first.end
            );
        }
        Ok(Self {
            inner: Mutex::new(Inner {
                file: opened.file,
                header: opened.header,
                data_start: opened.data_start,
                end: opened.end,
            }),
            path: path.to_path_buf(),
        })
    }

    pub fn store_id(&self) -> Uuid {
        self.inner.lock().unwrap().header.store_id
    }

    /// Position before the first record, as an index that has applied
    /// nothing records it.
    pub fn start(&self) -> JournalPosition {
        let inner = self.inner.lock().unwrap();
        JournalPosition {
            index_version: INDEX_VERSION,
            store_id: inner.header.store_id,
            generation: inner.header.generation,
            offset: inner.data_start,
        }
    }

    /// Whether the journal holds any record after its `FILE` record.
    pub fn is_empty(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.end == inner.data_start
    }

    /// Whether `position` was recorded against this journal file.
    pub fn matches(&self, position: &JournalPosition) -> bool {
        let inner = self.inner.lock().unwrap();
        position.index_version == INDEX_VERSION
            && position.store_id == inner.header.store_id
            && position.generation == inner.header.generation
            && position.offset >= inner.data_start
            && position.offset <= inner.end
    }

    /// Call `apply` with every record from `position` to the end, and the
    /// position just past it.
    pub fn replay<F>(&self, from: &JournalPosition, mut apply: F) -> Result<()>
    where
        F: FnMut(&JournalRecord, JournalPosition) -> Result<()>,
    {
        let mut inner = self.inner.lock().unwrap();
        let (store_id, generation, end) =
            (inner.header.store_id, inner.header.generation, inner.end);
        record_file::read_records(&mut inner.file, from.offset, end, |pos, frame, header| {
            let record = JournalRecord::decode(frame, header)
                .map_err(|err| anyhow!("journal record at offset {pos}: {err}"))?;
            apply(
                &record,
                JournalPosition {
                    index_version: INDEX_VERSION,
                    store_id,
                    generation,
                    offset: pos + frame.record_len(),
                },
            )
        })
    }

    /// Append `record` and fsync it, then call `apply` with the position just
    /// past it. The record is durable, and so committed, once the fsync
    /// returns. The lock is held across `apply` so the index applies
    /// records in journal order.
    pub fn commit<F>(&self, record: &JournalRecord, apply: F) -> Result<()>
    where
        F: FnOnce(JournalPosition) -> Result<()>,
    {
        let bytes = record
            .encode_record()
            .map_err(|err| anyhow!("can't encode journal record: {err}"))?;
        let mut inner = self.inner.lock().unwrap();
        let base = inner.end;
        let written: Result<()> = (|| {
            inner.file.seek(SeekFrom::Start(base))?;
            inner.file.write_all(&bytes)?;
            inner.file.sync_data()?;
            Ok(())
        })();
        if let Err(err) = written {
            // Leave nothing half-written for the next append to land after.
            let _ = inner.file.set_len(base);
            return Err(err);
        }
        inner.end = base + bytes.len() as u64;
        let position = JournalPosition {
            index_version: INDEX_VERSION,
            store_id: inner.header.store_id,
            generation: inner.header.generation,
            offset: inner.end,
        };
        apply(position)
    }

    /// Give an empty journal another store's id, so it pairs with an
    /// existing data log. Refused once the journal holds records.
    pub fn adopt_store_id(&self, store_id: Uuid) -> Result<JournalPosition> {
        let mut inner = self.inner.lock().unwrap();
        if inner.end != inner.data_start {
            bail!(
                "the capsule journal '{}' already holds records for store {}",
                self.path.display(),
                inner.header.store_id
            );
        }
        let header = FileHeader {
            store_id,
            generation: 0,
            ..inner.header
        };
        let end = record_file::write_new_file(&mut inner.file, &self.path, &header)?;
        inner.header = header;
        inner.data_start = end;
        inner.end = end;
        Ok(JournalPosition {
            index_version: INDEX_VERSION,
            store_id,
            generation: 0,
            offset: end,
        })
    }
}
