//! The capsule journal (`docs/on-disk-format.md` §5–§7): an append-only file
//! of `CAPSULE`, `DROP`, `NAMESPACE`, `BIND`, `UNBIND`, `KEY` and `BATCH`
//! records. It's the durable record of which capsules and names exist, and
//! of an encrypted store's key; sled is an index rebuilt from it.
//!
//! Records are never removed in place. Compaction (§9.2,
//! [`Journal::compact_with`]) writes the live state to the next generation
//! of the file and renames it over the old one.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, TryLockError};

use anyhow::{anyhow, bail, Context, Result};
use common::format::{FileHeader, FileKind, JournalRecord, StoreKey};
use common::CapsuleId;
use nvram_sim::record_file::{self, Swap, SwapFaults};
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

/// Version of the index sled keeps. Bump it whenever the encoding of an index
/// record changes or the index starts holding something new from the
/// journal: an index of another version is rebuilt from the journal.
///
/// 2: names (`NAMESPACE`, `BIND`, `UNBIND`) are indexed.
/// 3: a name's metadata (`BIND` layout version 2) is indexed with it.
/// 4: names are also indexed under the capsule they point at (#138).
pub const INDEX_VERSION: u32 = 4;

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

/// A journal's size, and how much of it a compaction would keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalUsage {
    /// Bytes in the journal.
    pub size: u64,
    /// Bytes the compacted journal would hold: its `FILE` record, the
    /// store's `KEY` record, then a record for each live namespace, capsule
    /// and name.
    pub live: u64,
}

impl JournalUsage {
    /// Bytes compaction would free. Compaction writes the members of a
    /// `BATCH` as records of their own, each with a frame, so a journal
    /// with little dead in it can compact to more than its size; that
    /// counts as nothing to free.
    pub fn reclaimable(&self) -> u64 {
        self.size.saturating_sub(self.live)
    }
}

/// What a journal compaction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalCompaction {
    /// Size of the journal it replaced, including records appended while
    /// it ran.
    pub before: u64,
    /// Size of the new generation.
    pub after: u64,
    /// The new generation.
    pub generation: u64,
}

impl JournalCompaction {
    pub fn reclaimed(&self) -> u64 {
        self.before.saturating_sub(self.after)
    }
}

/// Points in a compaction where a test can stop it, as a crash there would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The next generation holds the live state as of the start; records
    /// appended since haven't been copied.
    Written,
    /// The next generation is complete and fsynced, but not renamed.
    Synced,
    /// The next generation has replaced the journal on disk, and the index
    /// hasn't recorded it.
    Renamed,
}

struct Inner {
    file: File,
    header: FileHeader,
    data_start: u64,
    end: u64,
    /// Bytes compaction could free, estimated from what it measured last
    /// time and the records appended since; `None` until it has measured
    /// the journal. See [`Journal::reclaim_estimate`].
    dead_estimate: Option<u64>,
    /// The average length of a live `CAPSULE` record when compaction last
    /// measured the journal: what each `DROP` is estimated to free.
    capsule_record_len: u64,
    /// A compaction couldn't rename its next generation into place or
    /// reopen the journal. The handle is then to a removed file, holding
    /// other offsets, so appends, replays and compactions are refused until
    /// the store is opened again (#191).
    lost: bool,
    /// The last `KEY` record's key: the store's.
    key: Option<StoreKey>,
    /// Failures a test asks the compaction swap to make up.
    #[cfg(test)]
    swap_faults: SwapFaults,
}

impl Inner {
    fn ensure_not_lost(&self) -> Result<()> {
        if self.lost {
            bail!("the capsule journal was lost by a failed compaction; open the store again");
        }
        Ok(())
    }
}

pub struct Journal {
    inner: Mutex<Inner>,
    path: PathBuf,
    /// Held while a compaction runs, so only one runs at a time.
    compacting: Mutex<()>,
}

/// The records a record applies: those in a `BATCH`, or the record itself.
fn members(record: &JournalRecord) -> &[JournalRecord] {
    match record {
        JournalRecord::Batch(records) => records,
        other => std::slice::from_ref(other),
    }
}

/// A name's metadata (`docs/on-disk-format.md` §5.6).
type Metadata = Vec<(String, String)>;

/// The live state of a journal, as compaction keeps it. A capsule is held
/// as the offset of the record that holds its `CAPSULE`, which is read again
/// when the new generation is written, so no manifest stays in memory.
/// Namespaces and names are held whole.
#[derive(Default)]
struct Live {
    /// Capsule → (offset of the record holding its `CAPSULE`, length of the
    /// record compaction writes for it).
    capsules: HashMap<CapsuleId, (u64, u64)>,
    /// Namespace → (first creation time, record length).
    namespaces: BTreeMap<String, (u64, u64)>,
    /// (namespace, name) → (capsule, metadata, record length).
    names: BTreeMap<(String, String), (CapsuleId, Metadata, u64)>,
    /// The last `KEY` record's key, and its record length.
    key: Option<(StoreKey, u64)>,
}

impl Live {
    /// Apply the record at `pos` as the index does (`docs/on-disk-format.md`
    /// §7): a `CAPSULE` for a live capsule and a `DROP` of one that isn't
    /// live change nothing, and a namespace keeps its first creation time.
    fn apply(&mut self, pos: u64, record: &JournalRecord) -> Result<()> {
        for member in members(record) {
            let len = || {
                member
                    .record_len()
                    .map_err(|err| anyhow!("journal record at offset {pos}: {err}"))
            };
            match member {
                JournalRecord::Capsule(manifest) => {
                    if let Entry::Vacant(slot) = self.capsules.entry(manifest.capsule_id) {
                        slot.insert((pos, len()?));
                    }
                }
                JournalRecord::Drop(id) => {
                    self.capsules.remove(id);
                }
                JournalRecord::Namespace { ns, created_at } => {
                    if !self.namespaces.contains_key(ns) {
                        self.namespaces.insert(ns.clone(), (*created_at, len()?));
                    }
                }
                JournalRecord::DropNamespace { ns, .. } => {
                    self.namespaces.remove(ns);
                }
                JournalRecord::Bind {
                    ns,
                    name,
                    capsule_id,
                    metadata,
                } => {
                    self.names.insert(
                        (ns.clone(), name.clone()),
                        (*capsule_id, metadata.clone(), len()?),
                    );
                }
                JournalRecord::Unbind { ns, name } => {
                    self.names.remove(&(ns.clone(), name.clone()));
                }
                JournalRecord::Key(key) => self.key = Some((*key, len()?)),
                JournalRecord::Batch(_) => {
                    bail!("journal record at offset {pos}: a batch inside a batch")
                }
            }
        }
        Ok(())
    }

    fn capsule_bytes(&self) -> u64 {
        self.capsules.values().map(|(_, len)| len).sum()
    }

    /// Bytes of the records compaction writes for this state.
    fn bytes(&self) -> u64 {
        self.capsule_bytes()
            + self.key.map_or(0, |(_, len)| len)
            + self.namespaces.values().map(|(_, len)| len).sum::<u64>()
            + self.names.values().map(|(_, _, len)| len).sum::<u64>()
    }
}

fn put(out: &mut impl Write, written: &mut u64, record: &JournalRecord) -> Result<()> {
    let bytes = record
        .encode_record()
        .map_err(|err| anyhow!("can't encode journal record: {err}"))?;
    out.write_all(&bytes)?;
    *written += bytes.len() as u64;
    Ok(())
}

/// Write the store's `KEY` record, then a record for each live namespace,
/// capsule and name to `out`, after the `FILE` record that ends at
/// `data_start`. Each capsule's `CAPSULE` is
/// read again from `src`, so capsules keep their journal order. Returns
/// where the records end.
fn write_live(
    src: &mut File,
    out: &mut File,
    data_start: u64,
    snapshot_end: u64,
    live: &Live,
) -> Result<u64> {
    let mut w = BufWriter::new(out);
    let mut written = data_start;
    // Without it, the store would open without its key after compaction.
    if let Some((key, _)) = live.key {
        put(&mut w, &mut written, &JournalRecord::Key(key))?;
    }
    for (ns, (created_at, _)) in &live.namespaces {
        let record = JournalRecord::Namespace {
            ns: ns.clone(),
            created_at: *created_at,
        };
        put(&mut w, &mut written, &record)?;
    }
    let holders: HashSet<u64> = live.capsules.values().map(|(pos, _)| *pos).collect();
    record_file::read_records(src, data_start, snapshot_end, |pos, frame, header| {
        if !holders.contains(&pos) {
            return Ok(());
        }
        let record = JournalRecord::decode(frame, header)
            .map_err(|err| anyhow!("journal record at offset {pos}: {err}"))?;
        for member in members(&record) {
            if let JournalRecord::Capsule(manifest) = member {
                if live.capsules.get(&manifest.capsule_id).map(|(at, _)| *at) == Some(pos) {
                    put(&mut w, &mut written, member)?;
                }
            }
        }
        Ok(())
    })?;
    for ((ns, name), (capsule_id, metadata, _)) in &live.names {
        let record = JournalRecord::Bind {
            ns: ns.clone(),
            name: name.clone(),
            capsule_id: *capsule_id,
            metadata: metadata.clone(),
        };
        put(&mut w, &mut written, &record)?;
    }
    w.flush()?;
    Ok(written)
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
        // A compaction that didn't get as far as its rename leaves the next
        // generation behind. The journal it would have replaced is intact.
        record_file::remove_unfinished_compaction(path, FileKind::Journal)?;
        let mut key = None;
        let opened = record_file::open(path, FileKind::Journal, |pos, frame, header| {
            let record = JournalRecord::decode(frame, header)
                .map_err(|err| anyhow!("journal record at offset {pos}: {err}"))?;
            if let JournalRecord::Key(found) = record {
                key = Some(found);
            }
            Ok(())
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
                dead_estimate: None,
                capsule_record_len: 0,
                lost: false,
                key,
                #[cfg(test)]
                swap_faults: SwapFaults::default(),
            }),
            path: path.to_path_buf(),
            compacting: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn store_id(&self) -> Uuid {
        self.inner.lock().unwrap().header.store_id
    }

    /// The store's key, from the last `KEY` record. `None` for a store that
    /// has never been opened with one.
    pub fn store_key(&self) -> Option<StoreKey> {
        self.inner.lock().unwrap().key
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

    /// Whether the journal has never held a record: none follows its `FILE`
    /// record, and compaction has never replaced it. A journal compacted
    /// down to nothing keeps its store.
    pub fn is_empty(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.end == inner.data_start && inner.header.generation == 0
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

    /// The journal's size, and an estimate of the bytes compaction would
    /// free: what it found when it last measured the journal, plus every
    /// byte appended since, plus the average live `CAPSULE` record for each
    /// `DROP` among them. The estimate is `None` until compaction has
    /// measured the journal since it was opened.
    pub fn reclaim_estimate(&self) -> (u64, Option<u64>) {
        let inner = self.inner.lock().unwrap();
        (inner.end, inner.dead_estimate)
    }

    /// Call `apply` with every record from `position` to the end, and the
    /// position just past it.
    pub fn replay<F>(&self, from: &JournalPosition, mut apply: F) -> Result<()>
    where
        F: FnMut(&JournalRecord, JournalPosition) -> Result<()>,
    {
        let mut inner = self.inner.lock().unwrap();
        inner.ensure_not_lost()?;
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
    /// existing data log. Refused once the journal holds records, or has
    /// held them.
    pub fn adopt_store_id(&self, store_id: Uuid) -> Result<JournalPosition> {
        let mut inner = self.inner.lock().unwrap();
        if inner.end != inner.data_start || inner.header.generation != 0 {
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

    /// Replace the journal with the next generation of it, holding only the
    /// live state (`docs/on-disk-format.md` §9.2), if `worth` says the
    /// measured usage makes it worth it. Returns `None` if not.
    ///
    /// 1. Note where the journal ends.
    /// 2. Without the journal lock, read it to find the store's key and the
    ///    live capsules, namespaces and names, then write
    ///    `<journal>.compact`: a `FILE` record for the next generation, the
    ///    last `KEY` record, a `NAMESPACE` record for each live namespace,
    ///    the `CAPSULE` record of each live capsule in journal order, and a
    ///    `BIND` record for each name. Commits carry on.
    /// 3. Under the journal lock, copy the records committed since step 1
    ///    unchanged, fsync, rename the new file over the journal, and call
    ///    `on_swap` with the position at its end, for the index to record.
    ///
    /// A crash before the rename leaves `<journal>.compact`, which the next
    /// open removes; the old journal is intact. A crash after it leaves the
    /// complete new generation, and an index position from the old one, so
    /// the next open rebuilds the index from the new generation.
    ///
    /// `stop_at` is called at each [`Step`]. When it returns true,
    /// compaction stops there and leaves the files as a crash would. Only
    /// tests stop it; the journal must then be dropped and opened again.
    pub(crate) fn compact_with(
        &self,
        worth: impl FnOnce(&JournalUsage) -> bool,
        on_swap: impl FnOnce(JournalPosition) -> Result<()>,
        mut stop_at: impl FnMut(Step) -> bool,
    ) -> Result<Option<JournalCompaction>> {
        let _running = match self.compacting.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                bail!(
                    "a compaction of '{}' is already running",
                    self.path.display()
                )
            }
        };

        // 1. Where the journal ends. Only appends change it, and they land
        // after this point.
        let (header, data_start, snapshot_end) = {
            let inner = self.inner.lock().unwrap();
            inner.ensure_not_lost()?;
            (inner.header, inner.data_start, inner.end)
        };

        // 2. The live state, then the next generation. A handle of its own,
        // so commits needn't wait; it's closed before the rename.
        let mut src = File::open(&self.path)?;
        let mut live = Live::default();
        record_file::read_records(&mut src, data_start, snapshot_end, |pos, frame, header| {
            let record = JournalRecord::decode(frame, header)
                .map_err(|err| anyhow!("journal record at offset {pos}: {err}"))?;
            live.apply(pos, &record)
        })?;
        let usage = JournalUsage {
            size: snapshot_end,
            live: data_start + live.bytes(),
        };
        let capsule_record_len = live
            .capsule_bytes()
            .checked_div(live.capsules.len() as u64)
            .unwrap_or(0);
        if !worth(&usage) {
            let mut inner = self.inner.lock().unwrap();
            let appended = inner.end - snapshot_end;
            inner.dead_estimate = Some(usage.reclaimable() + appended);
            inner.capsule_record_len = capsule_record_len;
            return Ok(None);
        }

        let next_path = record_file::compaction_path(&self.path);
        let next = FileHeader {
            generation: header.generation + 1,
            ..header
        };
        let mut out = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&next_path)?;
        let written = (|| -> Result<u64> {
            let start = record_file::write_new_file(&mut out, &next_path, &next)?;
            if start != data_start {
                bail!("the next generation's FILE record is {start} bytes, not {data_start}");
            }
            let end = write_live(&mut src, &mut out, data_start, snapshot_end, &live)?;
            if end != usage.live {
                bail!(
                    "compaction wrote {end} bytes of journal where it expected {}",
                    usage.live
                );
            }
            Ok(end)
        })();
        drop(src);
        let written = match written {
            Ok(written) => written,
            Err(err) => {
                drop(out);
                let _ = std::fs::remove_file(&next_path);
                return Err(err);
            }
        };
        if stop_at(Step::Written) {
            bail!("compaction stopped at {:?}", Step::Written);
        }

        // 3. Catch up and swap, with commits held off.
        let mut inner = self.inner.lock().unwrap();
        let old_end = inner.end;
        let caught_up = (|| -> Result<()> {
            out.seek(SeekFrom::Start(written))?;
            record_file::copy_range(&mut inner.file, &mut out, snapshot_end, old_end)?;
            out.sync_all()?;
            Ok(())
        })();
        if let Err(err) = caught_up {
            drop(out);
            let _ = std::fs::remove_file(&next_path);
            return Err(err);
        }
        if stop_at(Step::Synced) {
            bail!("compaction stopped at {:?}", Step::Synced);
        }

        #[cfg(test)]
        let faults = inner.swap_faults;
        #[cfg(not(test))]
        let faults = SwapFaults::default();
        match record_file::swap_in_next_generation(
            &self.path,
            &mut inner.file,
            out,
            &next_path,
            faults,
        ) {
            Swap::Swapped => {}
            Swap::Unchanged(err) => bail!(
                "couldn't move the compacted journal into place: {err}; the journal is unchanged"
            ),
            Swap::Lost { rename, reopen } => {
                // The handle left is to the file just removed, so anything
                // appended to it would be lost.
                inner.lost = true;
                bail!(
                    "couldn't move the compacted journal into place ({rename}) or reopen the \
                     old one ({reopen}); commits are refused until the store is opened again"
                );
            }
        }
        if stop_at(Step::Renamed) {
            bail!("compaction stopped at {:?}", Step::Renamed);
        }

        inner.header = next;
        inner.end = written + (old_end - snapshot_end);
        inner.dead_estimate = Some(old_end - snapshot_end);
        inner.capsule_record_len = capsule_record_len;
        let done = JournalCompaction {
            before: old_end,
            after: inner.end,
            generation: next.generation,
        };
        on_swap(JournalPosition {
            index_version: INDEX_VERSION,
            store_id: next.store_id,
            generation: next.generation,
            offset: inner.end,
        })
        .context(
            "the journal was compacted, but the index couldn't record its new generation; the \
             next open rebuilds the index from it",
        )?;
        drop(inner);
        info!(
            path = %self.path.display(),
            generation = done.generation,
            before = done.before,
            after = done.after,
            "compacted the capsule journal"
        );
        Ok(Some(done))
    }
}

/// The journal, locked by [`Journal::lock`].
pub struct JournalWriter<'a> {
    inner: MutexGuard<'a, Inner>,
}

impl JournalWriter<'_> {
    /// The store's key, as [`Journal::store_key`].
    pub fn store_key(&self) -> Option<StoreKey> {
        self.inner.key
    }

    /// Append `record` and fsync it. The record is durable, and so
    /// committed, once this returns. Returns the position just past it.
    pub fn append(&mut self, record: &JournalRecord) -> Result<JournalPosition> {
        let bytes = record
            .encode_record()
            .map_err(|err| anyhow!("can't encode journal record: {err}"))?;
        let inner = &mut *self.inner;
        inner.ensure_not_lost()?;
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
        if let JournalRecord::Key(key) = record {
            inner.key = Some(*key);
        }
        let per_drop = inner.capsule_record_len;
        if let Some(dead) = inner.dead_estimate.as_mut() {
            let drops = members(record)
                .iter()
                .filter(|r| matches!(r, JournalRecord::Drop(_)))
                .count() as u64;
            *dead = dead
                .saturating_add(bytes.len() as u64)
                .saturating_add(drops.saturating_mul(per_drop));
        }
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

    /// A DROP record whose bytes are valid UTF-8, so a name can hold it.
    fn record_a_name_can_hold() -> String {
        (0u64..)
            .find_map(|n| {
                let id: [u8; 16] = format!("{n:016}").as_bytes().try_into().unwrap();
                let record = JournalRecord::Drop(CapsuleId::from_uuid(Uuid::from_bytes(id)));
                String::from_utf8(record.encode_record().unwrap()).ok()
            })
            .unwrap()
    }

    fn scratch_journal(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("space_journal_{name}_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("space.journal");
        (dir, path)
    }

    /// A name or metadata value can hold the bytes of a whole record. A BIND
    /// carrying one, cut short at any byte, is still a torn tail and not
    /// damage, so the journal opens (#137).
    #[test]
    fn a_bind_holding_a_record_cut_at_any_byte_is_a_torn_tail() {
        let (dir, path) = scratch_journal("embedded");
        let embedded = record_a_name_can_hold();
        let kept = manifest();
        let (clean_len, bytes) = {
            let journal = Journal::open(&path).unwrap();
            let mut writer = journal.lock();
            writer
                .append(&JournalRecord::Capsule(Box::new(kept.clone())))
                .unwrap();
            let clean_len = std::fs::metadata(&path).unwrap().len();
            for record in [
                JournalRecord::Bind {
                    ns: "s3:b".into(),
                    name: format!("key {embedded} tail"),
                    capsule_id: kept.capsule_id,
                    metadata: vec![("x-amz-meta-a".into(), embedded.clone())],
                },
                JournalRecord::Batch(vec![JournalRecord::Unbind {
                    ns: "s3:b".into(),
                    name: format!("{embedded}{embedded}"),
                }]),
            ] {
                writer.append(&record).unwrap();
            }
            (clean_len as usize, std::fs::read(&path).unwrap())
        };
        let bind_len = JournalRecord::Bind {
            ns: "s3:b".into(),
            name: format!("key {embedded} tail"),
            capsule_id: kept.capsule_id,
            metadata: vec![("x-amz-meta-a".into(), embedded.clone())],
        }
        .record_len()
        .unwrap() as usize;

        // Cut inside the BIND, and inside the BATCH after it.
        for cut in (clean_len + 1..bytes.len()).filter(|cut| *cut != clean_len + bind_len) {
            let kept_len = if cut < clean_len + bind_len {
                clean_len
            } else {
                clean_len + bind_len
            };
            std::fs::write(&path, &bytes[..cut]).unwrap();
            let journal =
                Journal::open(&path).unwrap_or_else(|err| panic!("cut at {cut}: {err:#}"));
            assert_eq!(
                std::fs::metadata(&path).unwrap().len() as usize,
                kept_len,
                "cut at {cut}"
            );
            let mut records = 0;
            journal
                .replay(&journal.start(), |_, _| {
                    records += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                records,
                1 + usize::from(kept_len > clean_len),
                "cut at {cut}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A flipped bit in a record's header length makes it claim to run
    /// past the end of the file. Its header is whole, which an append cut
    /// short can't leave, so it's damage and the journal refuses to open,
    /// whether records follow it or not. Nothing is truncated (#137).
    #[test]
    fn a_record_whose_length_is_damaged_is_not_taken_for_a_torn_tail() {
        let bind = |name: &str| JournalRecord::Bind {
            ns: "s3:b".into(),
            name: name.into(),
            capsule_id: CapsuleId::from_uuid(Uuid::from_u128(3)),
            metadata: Vec::new(),
        };
        for records_after in [2, 0] {
            let (dir, path) = scratch_journal("damaged_length");
            let damaged_at = {
                let journal = Journal::open(&path).unwrap();
                let mut writer = journal.lock();
                writer.append(&bind("first")).unwrap();
                let at = std::fs::metadata(&path).unwrap().len() as usize;
                writer.append(&bind("damaged")).unwrap();
                for i in 0..records_after {
                    writer.append(&bind(&format!("after {i}"))).unwrap();
                }
                at
            };
            let mut bytes = std::fs::read(&path).unwrap();
            // header_len is bytes 8..12 of the frame.
            bytes[damaged_at + 10] ^= 0x04;
            std::fs::write(&path, &bytes).unwrap();

            let err = Journal::open(&path).err().unwrap();
            assert!(
                err.to_string()
                    .contains(&format!("damaged at bytes {damaged_at}..")),
                "{records_after} after: {err:#}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{records_after} after"
            );
            let _ = std::fs::remove_dir_all(&dir);
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
                        metadata: Vec::new(),
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

/// Journal compaction through the registry (#136): the compacted journal
/// holds only the live state, an index rebuilt from it is the same index,
/// and a crash at any step opens with no manual step.
#[cfg(test)]
mod compaction_tests {
    use super::*;
    use crate::{CapsuleRegistry, JOURNAL_MIN_RECLAIM};
    use common::format::{CapsuleManifest, ManifestEntry};
    use common::{CapsuleId, Policy, SegmentId};
    use std::fs;

    /// Namespaces the workload uses. The last is never created.
    const NAMESPACES: [&str; 3] = ["s3:a", "s3:b", "s3:never"];

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("space_journal_compaction")
            .join(format!("{name}_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A manifest of one to four entries drawn from 40 segments. A segment's
    /// content hash follows from its id, as dedup makes it in a real store,
    /// so capsules share segments the way dedup hits do.
    fn manifest(rng: &mut Rng) -> CapsuleManifest {
        let entries: Vec<ManifestEntry> = (0..1 + rng.below(4))
            .map(|_| {
                let segment = 1 + rng.below(40) as u64;
                ManifestEntry {
                    segment_id: SegmentId(segment),
                    plain_len: 100 + segment as u32,
                    content_hash: [segment as u8; 32],
                }
            })
            .collect();
        CapsuleManifest {
            capsule_id: CapsuleId::new(),
            created_at: 1_700_000_000 + rng.below(1000) as u64,
            size: entries.iter().map(|e| u64::from(e.plain_len)).sum(),
            digest: [7u8; 32],
            dedupe: rng.below(5) != 0,
            requires_encryption: false,
            deduped_bytes: 0,
            policy: Policy::default(),
            entries,
        }
    }

    fn take(live: &mut Vec<CapsuleId>, id: CapsuleId) -> bool {
        match live.iter().position(|c| *c == id) {
            Some(i) => {
                live.swap_remove(i);
                true
            }
            None => false,
        }
    }

    /// Commit `ops` of everything the journal records: writes that share
    /// segments, deletes, S3 puts, overwrites and deletes, namespaces
    /// created more than once, names bound to dropped capsules, and unbinds
    /// of names that aren't bound.
    fn churn(registry: &CapsuleRegistry, rng: &mut Rng, ops: usize) {
        let mut live: Vec<CapsuleId> = registry.capsules().into_iter().map(|c| c.id).collect();
        let mut dropped: Vec<CapsuleId> = Vec::new();
        for _ in 0..ops {
            let ns = NAMESPACES[rng.below(2)].to_string();
            let name = format!("k{}", rng.below(12));
            let held = registry.name(&ns, &name).unwrap();
            match rng.below(8) {
                0 | 1 => {
                    let m = manifest(rng);
                    live.push(m.capsule_id);
                    registry.create_capsule(m).unwrap();
                }
                2 if !live.is_empty() => {
                    let id = live.swap_remove(rng.below(live.len()));
                    registry.delete_capsule(id).unwrap();
                    dropped.push(id);
                }
                3 => {
                    // Mostly creates, sometimes a delete. Its names stay.
                    let created_at = rng.below(100) as u64;
                    let record = if created_at < 20 {
                        JournalRecord::DropNamespace {
                            ns,
                            dropped_at: created_at,
                        }
                    } else {
                        JournalRecord::Namespace { ns, created_at }
                    };
                    registry.commit(vec![record]).unwrap();
                }
                4 => {
                    // An S3 PUT: a new key, or an overwrite that drops the
                    // capsule the key held.
                    let m = manifest(rng);
                    let id = m.capsule_id;
                    // Metadata that differs between overwrites of a key.
                    let metadata = vec![("content-type".to_string(), format!("type/{id:?}"))];
                    let mut records = vec![
                        JournalRecord::Capsule(Box::new(m)),
                        JournalRecord::Bind {
                            ns,
                            name,
                            capsule_id: id,
                            metadata,
                        },
                    ];
                    if let Some(old) = held.filter(|old| take(&mut live, *old)) {
                        records.push(JournalRecord::Drop(old));
                        dropped.push(old);
                    }
                    registry.commit(records).unwrap();
                    live.push(id);
                }
                5 => {
                    // An S3 DELETE.
                    let mut records = vec![JournalRecord::Unbind { ns, name }];
                    if let Some(old) = held.filter(|old| take(&mut live, *old)) {
                        records.push(JournalRecord::Drop(old));
                        dropped.push(old);
                    }
                    registry.commit(records).unwrap();
                }
                6 => {
                    let pool = if rng.below(2) == 0 { &dropped } else { &live };
                    if !pool.is_empty() {
                        let capsule_id = pool[rng.below(pool.len())];
                        registry
                            .commit(vec![JournalRecord::Bind {
                                ns,
                                name,
                                capsule_id,
                                metadata: Vec::new(),
                            }])
                            .unwrap();
                    }
                }
                _ => {
                    registry
                        .commit(vec![JournalRecord::Unbind { ns, name }])
                        .unwrap();
                }
            }
        }
    }

    /// Everything the index holds that the journal rebuilds.
    #[derive(Debug, PartialEq)]
    struct Index {
        capsules: Vec<String>,
        refs: Vec<(u64, u32)>,
        content: Vec<(String, u64)>,
        namespaces: Vec<Option<u64>>,
        names: Vec<Vec<(String, crate::Binding)>>,
        /// Each live capsule and the names under it.
        capsule_names: Vec<(String, Vec<(String, String)>)>,
    }

    fn index(registry: &CapsuleRegistry) -> Index {
        let mut capsules: Vec<String> = registry
            .capsules()
            .iter()
            .map(|c| format!("{c:?}"))
            .collect();
        capsules.sort();
        let mut refs: Vec<(u64, u32)> = registry
            .segment_refs()
            .unwrap()
            .into_iter()
            .map(|(segment, count)| (segment.0, count))
            .collect();
        refs.sort();
        let mut content: Vec<(String, u64)> = registry
            .content_entries()
            .into_iter()
            .map(|(hash, segment)| (hash.0, segment.0))
            .collect();
        content.sort();
        let mut capsule_names: Vec<(String, Vec<(String, String)>)> = registry
            .capsules()
            .iter()
            .map(|c| {
                let mut held = registry.names_of(c.id).unwrap();
                held.sort();
                (c.id.as_uuid().to_string(), held)
            })
            .collect();
        capsule_names.sort();
        Index {
            capsules,
            refs,
            content,
            namespaces: NAMESPACES
                .iter()
                .map(|ns| registry.namespace_created_at(ns).unwrap())
                .collect(),
            names: NAMESPACES
                .iter()
                .map(|ns| registry.list_names(ns, "", None, usize::MAX).unwrap())
                .collect(),
            capsule_names,
        }
    }

    fn always(usage: &JournalUsage) -> bool {
        usage.reclaimable() > 0
    }

    #[test]
    fn compaction_keeps_only_the_live_state() {
        let dir = scratch("live");
        let db = dir.join("space.db");
        let path = Journal::path_for(&db);
        let registry = CapsuleRegistry::open(&db).unwrap();
        let mut rng = Rng(0x5eed);
        churn(&registry, &mut rng, 400);
        let before = index(&registry);
        assert!(!before.capsules.is_empty() && !before.names[0].is_empty());
        let size = fs::metadata(&path).unwrap().len();

        let done = registry
            .compact_journal()
            .unwrap()
            .expect("nothing reclaimed");
        assert_eq!((done.before, done.generation), (size, 1));
        assert!(done.after < size, "{done:?}");
        assert_eq!(fs::metadata(&path).unwrap().len(), done.after);
        assert_eq!(index(&registry), before);

        // One record for each live capsule, namespace and name.
        let (mut capsules, mut namespaces, mut names) = (0, 0, 0);
        registry
            .journal
            .replay(&registry.journal.start(), |record, _| {
                match record {
                    JournalRecord::Capsule(_) => capsules += 1,
                    JournalRecord::Namespace { .. } => namespaces += 1,
                    JournalRecord::Bind { .. } => names += 1,
                    other => panic!("compaction kept {other:?}"),
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(capsules, before.capsules.len());
        assert_eq!(
            namespaces,
            before.namespaces.iter().filter(|t| t.is_some()).count()
        );
        assert_eq!(names, before.names.iter().map(Vec::len).sum::<usize>());

        // The index has recorded the new generation, so nothing replays.
        let position = registry.store.journal_position().unwrap().unwrap();
        assert_eq!((position.generation, position.offset), (1, done.after));
        assert!(registry.compact_journal().unwrap().is_none());

        // Commits land in the new generation.
        churn(&registry, &mut rng, 50);
        let after = index(&registry);
        drop(registry);
        let registry = CapsuleRegistry::open(&db).unwrap();
        assert_eq!(index(&registry), after);
        drop(registry);

        // An index rebuilt from the compacted journal is the same index.
        fs::remove_dir_all(&db).unwrap();
        let registry = CapsuleRegistry::open(&db).unwrap();
        assert_eq!(index(&registry), after);

        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_at_any_step_of_compaction_opens_with_the_same_state() {
        for step in [Step::Written, Step::Synced, Step::Renamed] {
            let dir = scratch("crash");
            let db = dir.join("space.db");
            let unfinished = record_file::compaction_path(&Journal::path_for(&db));
            let registry = CapsuleRegistry::open(&db).unwrap();
            churn(&registry, &mut Rng(7), 300);
            let before = index(&registry);

            let err = registry
                .compact_journal_with(always, |at| at == step)
                .unwrap_err();
            assert!(err.to_string().contains("stopped"), "{step:?}: {err:#}");
            let renamed = step == Step::Renamed;
            assert_eq!(unfinished.exists(), !renamed, "{step:?}");
            drop(registry);

            // Opening removes an unfinished generation. One that was renamed
            // into place doesn't match the index's position, so the index
            // is rebuilt from it.
            let registry =
                CapsuleRegistry::open(&db).unwrap_or_else(|err| panic!("{step:?}: {err:#}"));
            assert!(!unfinished.exists(), "{step:?}");
            assert_eq!(
                registry.journal.start().generation,
                u64::from(renamed),
                "{step:?}"
            );
            assert_eq!(index(&registry), before, "{step:?}");

            churn(&registry, &mut Rng(8), 20);
            assert!(registry.compact_journal().unwrap().is_some(), "{step:?}");
            drop(registry);
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_torn_compaction_file_is_removed_on_open() {
        let dir = scratch("torn");
        let db = dir.join("space.db");
        let unfinished = record_file::compaction_path(&Journal::path_for(&db));
        let registry = CapsuleRegistry::open(&db).unwrap();
        churn(&registry, &mut Rng(9), 50);
        let before = index(&registry);
        drop(registry);

        fs::write(&unfinished, b"SPCR, cut short").unwrap();
        let registry = CapsuleRegistry::open(&db).unwrap();
        assert!(!unfinished.exists());
        assert_eq!(index(&registry), before);

        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A swap whose rename fails leaves the journal as it was, and usable
    /// (#191).
    #[test]
    fn a_failed_rename_leaves_the_journal_usable() {
        let dir = scratch("rename_fails");
        let db = dir.join("space.db");
        let path = Journal::path_for(&db);
        let registry = CapsuleRegistry::open(&db).unwrap();
        churn(&registry, &mut Rng(13), 200);
        let before = index(&registry);
        let size = fs::metadata(&path).unwrap().len();
        registry.journal.inner.lock().unwrap().swap_faults.rename = true;

        let err = registry.compact_journal().unwrap_err();
        assert!(
            err.to_string().contains("the journal is unchanged"),
            "{err:#}"
        );
        assert!(!record_file::compaction_path(&path).exists());
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        assert_eq!(registry.journal.start().generation, 0);
        assert_eq!(index(&registry), before);

        // Commits carry on, and it compacts once the rename works.
        churn(&registry, &mut Rng(14), 20);
        registry.journal.inner.lock().unwrap().swap_faults.rename = false;
        assert!(registry.compact_journal().unwrap().is_some());
        let after = index(&registry);
        drop(registry);
        let registry = CapsuleRegistry::open(&db).unwrap();
        assert_eq!(registry.journal.start().generation, 1);
        assert_eq!(index(&registry), after);
        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A swap that can't rename or reopen loses the journal's handle.
    /// Commits, replays and further compactions are refused until the store
    /// is opened again, which finds the old journal intact (#191).
    #[test]
    fn a_swap_that_cant_recover_refuses_commits_and_compactions() {
        let dir = scratch("swap_lost");
        let db = dir.join("space.db");
        let registry = CapsuleRegistry::open(&db).unwrap();
        churn(&registry, &mut Rng(15), 200);
        let before = index(&registry);
        registry.journal.inner.lock().unwrap().swap_faults = SwapFaults {
            rename: true,
            reopen: true,
        };

        let err = registry.compact_journal().unwrap_err();
        assert!(
            err.to_string()
                .contains("commits are refused until the store is opened again"),
            "{err:#}"
        );
        let lost = |err: anyhow::Error| err.to_string().contains("lost by a failed compaction");
        let created = registry.create_capsule(manifest(&mut Rng(16)));
        assert!(lost(created.unwrap_err()));
        let namespace = JournalRecord::Namespace {
            ns: NAMESPACES[0].to_string(),
            created_at: 1,
        };
        assert!(lost(registry.commit(vec![namespace]).unwrap_err()));
        let replayed = registry
            .journal
            .replay(&registry.journal.start(), |_, _| Ok(()));
        assert!(lost(replayed.unwrap_err()));
        assert!(lost(registry.compact_journal().unwrap_err()));
        assert_eq!(index(&registry), before);
        drop(registry);

        // The old journal is intact, and opening the store again recovers.
        let registry = CapsuleRegistry::open(&db).unwrap();
        assert_eq!(registry.journal.start().generation, 0);
        assert_eq!(index(&registry), before);
        churn(&registry, &mut Rng(17), 20);
        assert!(registry.compact_journal().unwrap().is_some());
        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_committed_while_compaction_runs_are_kept() {
        let dir = scratch("concurrent");
        let db = dir.join("space.db");
        let path = Journal::path_for(&db);
        let registry = CapsuleRegistry::open(&db).unwrap();
        churn(&registry, &mut Rng(11), 300);

        let other = registry.clone();
        let mut rng = Rng(12);
        let done = registry
            .compact_journal_with(always, |step| {
                if step == Step::Written {
                    churn(&other, &mut rng, 60);
                }
                false
            })
            .unwrap()
            .expect("nothing reclaimed");
        drop(other);
        let expected = index(&registry);
        assert_eq!(fs::metadata(&path).unwrap().len(), done.after);
        let position = registry.store.journal_position().unwrap().unwrap();
        assert_eq!((position.generation, position.offset), (1, done.after));
        drop(registry);

        let registry = CapsuleRegistry::open(&db).unwrap();
        assert_eq!(index(&registry), expected);
        drop(registry);
        fs::remove_dir_all(&db).unwrap();
        let registry = CapsuleRegistry::open(&db).unwrap();
        assert_eq!(index(&registry), expected);

        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scheduled_pass_compacts_once_a_quarter_of_the_journal_is_dead() {
        let dir = scratch("threshold");
        let db = dir.join("space.db");
        let registry = CapsuleRegistry::open(&db).unwrap();

        // A journal under 1 MiB isn't worth reading.
        registry
            .commit(vec![JournalRecord::Bind {
                ns: "s3:b".into(),
                name: "kept".into(),
                capsule_id: CapsuleId::new(),
                metadata: Vec::new(),
            }])
            .unwrap();
        assert!(registry.compact_journal_if_worthwhile().unwrap().is_none());
        assert_eq!(registry.journal.reclaim_estimate().1, None);

        // A capsule whose manifest is over 1 MiB on its own. The first pass
        // over 1 MiB measures the journal, and finds nothing dead.
        let entries = vec![
            ManifestEntry {
                segment_id: SegmentId(1),
                plain_len: 1,
                content_hash: [0u8; 32],
            };
            24_000
        ];
        let big = CapsuleManifest {
            size: entries.len() as u64,
            entries,
            dedupe: false,
            ..manifest(&mut Rng(13))
        };
        let big_id = big.capsule_id;
        registry.create_capsule(big).unwrap();
        assert!(registry.compact_journal_if_worthwhile().unwrap().is_none());
        assert_eq!(registry.journal.reclaim_estimate().1, Some(0));

        // A DROP is a small record, but the estimate counts what it frees.
        registry.delete_capsule(big_id).unwrap();
        let done = registry
            .compact_journal_if_worthwhile()
            .unwrap()
            .expect("not compacted");
        assert!(done.reclaimed() >= JOURNAL_MIN_RECLAIM, "{done:?}");
        assert!(registry.compact_journal_if_worthwhile().unwrap().is_none());
        assert!(registry.name("s3:b", "kept").unwrap().is_some());

        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_journal_compacted_to_nothing_keeps_its_store() {
        let dir = scratch("empty");
        let db = dir.join("space.db");
        let registry = CapsuleRegistry::open(&db).unwrap();
        let m = manifest(&mut Rng(14));
        let id = m.capsule_id;
        registry.create_capsule(m).unwrap();
        registry.delete_capsule(id).unwrap();

        let done = registry
            .compact_journal()
            .unwrap()
            .expect("nothing reclaimed");
        assert_eq!(done.after, registry.journal.start().offset);
        let err = registry.bind_store(Uuid::new_v4(), false).unwrap_err();
        assert!(err.to_string().contains("different stores"), "{err:#}");

        drop(registry);
        let _ = fs::remove_dir_all(&dir);
    }
}
