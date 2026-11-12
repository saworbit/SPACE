//! The capsule journal (`docs/on-disk-format.md` §5–§7): an append-only file
//! of `CAPSULE`, `DROP`, `NAMESPACE`, `BIND`, `UNBIND` and `BATCH` records.
//! It's the durable record of which capsules and names exist; sled is an
//! index rebuilt from it.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{anyhow, bail, Result};
use common::format::{FileHeader, FileKind, JournalRecord};
use nvram_sim::record_file;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version of the index sled keeps. Bump it whenever the encoding of an index
/// record changes or the index starts holding something new from the
/// journal: an index of another version is rebuilt from the journal.
///
/// 2: names (`NAMESPACE`, `BIND`, `UNBIND`) are indexed.
pub const INDEX_VERSION: u32 = 2;

/// How far into which journal the index has applied records. Stored in
/// sled next to the index, and updated in the same transaction as each
/// record's effects.
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

    /// Lock the journal for appending. Hold the lock across checking the
    /// index, appending and applying: the index then applies records in
    /// journal order, and a check sees every earlier commit.
    pub fn lock(&self) -> JournalWriter<'_> {
        JournalWriter {
            inner: self.inner.lock().unwrap(),
        }
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

/// The journal, locked by [`Journal::lock`].
pub struct JournalWriter<'a> {
    inner: MutexGuard<'a, Inner>,
}

impl JournalWriter<'_> {
    /// Append `record` and fsync it. The record is durable, and so
    /// committed, once this returns. Returns the position just past it.
    pub fn append(&mut self, record: &JournalRecord) -> Result<JournalPosition> {
        let bytes = record
            .encode_record()
            .map_err(|err| anyhow!("can't encode journal record: {err}"))?;
        let inner = &mut *self.inner;
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
        Ok(JournalPosition {
            index_version: INDEX_VERSION,
            store_id: inner.header.store_id,
            generation: inner.header.generation,
            offset: inner.end,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::format::{CapsuleManifest, ManifestEntry};
    use common::{CapsuleId, Policy, SegmentId};

    fn manifest() -> CapsuleManifest {
        CapsuleManifest {
            capsule_id: CapsuleId::new(),
            created_at: 1_700_000_000,
            size: 10,
            digest: [1u8; 32],
            dedupe: true,
            requires_encryption: false,
            deduped_bytes: 0,
            policy: Policy::default(),
            entries: vec![ManifestEntry {
                segment_id: SegmentId(1),
                plain_len: 10,
                content_hash: [2u8; 32],
            }],
        }
    }

    #[test]
    fn a_batch_cut_at_any_byte_is_dropped_as_a_torn_tail() {
        let dir = std::env::temp_dir().join(format!("space_journal_unit_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("space.journal");

        let old = manifest();
        let new = manifest();
        let (clean_len, bytes) = {
            let journal = Journal::open(&path).unwrap();
            let mut writer = journal.lock();
            writer
                .append(&JournalRecord::Capsule(Box::new(old.clone())))
                .unwrap();
            let clean_len = std::fs::metadata(&path).unwrap().len();
            writer
                .append(&JournalRecord::Batch(vec![
                    JournalRecord::Capsule(Box::new(new.clone())),
                    JournalRecord::Bind {
                        ns: "s3:b".into(),
                        name: "k".into(),
                        capsule_id: new.capsule_id,
                    },
                    JournalRecord::Drop(old.capsule_id),
                ]))
                .unwrap();
            (clean_len as usize, std::fs::read(&path).unwrap())
        };

        for cut in clean_len + 1..bytes.len() {
            std::fs::write(&path, &bytes[..cut]).unwrap();
            let journal =
                Journal::open(&path).unwrap_or_else(|err| panic!("cut at {cut}: {err:#}"));
            assert_eq!(
                std::fs::metadata(&path).unwrap().len() as usize,
                clean_len,
                "cut at {cut}"
            );
            let mut records = 0;
            journal
                .replay(&journal.start(), |_, _| {
                    records += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(records, 1, "cut at {cut}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
