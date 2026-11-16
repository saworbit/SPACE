//! S3 multipart upload (#201). An open upload is a namespace in the capsule
//! journal, `mpu:<upload id>`, so it survives a restart and an index
//! rebuild, and journal compaction keeps it while it's open:
//!
//! - its name `""` points at no capsule. Its metadata holds the bucket
//!   (`:bucket`), the key (`:key`) and the headers CreateMultipartUpload
//!   was sent, which the object gets;
//! - each part is a capsule, stored as a PUT body is, digests and all, and
//!   bound as `part:NNNNN`, its number in five digits.
//!
//! CompleteMultipartUpload reads the parts it names, in order, to compute
//! the object's digest, then commits one batch: a capsule made of the
//! parts' segments, the key's `BIND`, the drop of the object the key held,
//! and the removal of every part, the upload's name and its namespace. The
//! parts' bytes aren't written again (`docs/on-disk-format.md` §6).
//! AbortMultipartUpload removes the parts, the name and the namespace, as
//! DeleteBucket does for each upload into the bucket, and a reclaim pass
//! aborts uploads older than [`ABANDONED_UPLOAD_AGE`]. ListMultipartUploads
//! reads every upload's name to find those into a bucket.

use std::time::Duration;

use common::SegmentId;

use super::*;

/// The namespaces of open uploads start with this.
pub const UPLOAD_PREFIX: &str = "mpu:";
/// As in S3, every part but the last must be at least 5 MiB.
pub const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;
/// As in S3, parts are numbered 1 to 10000.
pub const MAX_PART_NUMBER: u32 = 10_000;
/// A reclaim pass aborts uploads started longer ago than this.
pub const ABANDONED_UPLOAD_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const BUCKET_FIELD: &str = ":bucket";
const KEY_FIELD: &str = ":key";
const PART_PREFIX: &str = "part:";

fn upload_namespace(upload_id: &str) -> String {
    format!("{UPLOAD_PREFIX}{upload_id}")
}

fn part_name(number: u32) -> String {
    format!("{PART_PREFIX}{number:05}")
}

/// A part of an open upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub number: u32,
    pub capsule_id: CapsuleId,
    pub size: u64,
    pub created_at: u64,
}

/// An open upload, as ListMultipartUploads lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenUpload {
    pub key: String,
    pub upload_id: String,
    /// When CreateMultipartUpload started it, in seconds since the epoch.
    pub initiated: u64,
}

/// A page of ListMultipartUploads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UploadPage {
    pub uploads: Vec<OpenUpload>,
    /// With a delimiter, the common prefixes that the keys of the uploads
    /// left out of `uploads` were rolled into (#253).
    pub common_prefixes: Vec<String>,
    /// Whether more uploads or common prefixes follow the last one listed.
    pub truncated: bool,
    /// The key or common prefix listed last: the key marker for the next
    /// page.
    pub next_key_marker: Option<String>,
    /// The id of the upload listed last, unless a common prefix came after
    /// it: the upload id marker for the next page.
    pub next_upload_id_marker: Option<String>,
}

/// What the journal holds for an open upload.
struct Upload {
    ns: String,
    bucket: String,
    key: String,
    /// The headers the object gets.
    metadata: Vec<(String, String)>,
    /// Each bound part: its number and capsule, by number.
    parts: Vec<(u32, CapsuleId)>,
}

impl S3View {
    /// The open upload in namespace `ns`, if there is one.
    fn upload_at(&self, ns: &str) -> S3Result<Option<Upload>> {
        let Some(binding) = self.registry.binding(ns, "")? else {
            return Ok(None);
        };
        let field = |name: &str| {
            binding
                .metadata
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let (bucket, key) = (field(BUCKET_FIELD), field(KEY_FIELD));
        let metadata = binding
            .metadata
            .into_iter()
            .filter(|(k, _)| !k.starts_with(':'))
            .collect();
        let parts = self
            .registry
            .list_names(ns, PART_PREFIX, None, usize::MAX)?
            .into_iter()
            .filter_map(|(name, binding)| {
                let number = name.strip_prefix(PART_PREFIX)?.parse().ok()?;
                Some((number, binding.capsule_id))
            })
            .collect();
        Ok(Some(Upload {
            ns: ns.to_string(),
            bucket,
            key,
            metadata,
            parts,
        }))
    }

    /// The open upload `upload_id` of `key` in `bucket`. Call it with the
    /// pipeline locked when the answer has to hold until a commit.
    fn upload(&self, bucket: &str, key: &str, upload_id: &str) -> S3Result<Upload> {
        let missing = || S3Error::NoSuchUpload(upload_id.to_string());
        // Upload ids are 32 hex digits; nothing else names one.
        if upload_id.is_empty()
            || upload_id.len() > 64
            || !upload_id.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(missing());
        }
        match self.upload_at(&upload_namespace(upload_id))? {
            Some(upload) if upload.bucket == bucket && upload.key == key => Ok(upload),
            _ => Err(missing()),
        }
    }

    /// The open uploads into `bucket`, by key and then upload id, which for
    /// one key is the order they were started in. Uploads started before
    /// #253 have random ids, and sort among the others by them. Nothing
    /// indexes uploads by bucket, so this reads every open upload's name.
    fn uploads_into(&self, bucket: &str) -> S3Result<Vec<OpenUpload>> {
        let mut uploads = Vec::new();
        for (ns, initiated) in self
            .registry
            .list_namespaces(UPLOAD_PREFIX, None, usize::MAX)?
        {
            let Some(binding) = self.registry.binding(&ns, "")? else {
                continue;
            };
            let field = |name: &str| {
                binding
                    .metadata
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
            };
            if field(BUCKET_FIELD) != Some(bucket) {
                continue;
            }
            uploads.push(OpenUpload {
                key: field(KEY_FIELD).unwrap_or_default().to_string(),
                upload_id: ns[UPLOAD_PREFIX.len()..].to_string(),
                initiated,
            });
        }
        uploads.sort_by(|a, b| (&a.key, &a.upload_id).cmp(&(&b.key, &b.upload_id)));
        Ok(uploads)
    }

    /// The records that abort every open upload into `bucket`, for
    /// DeleteBucket to commit with the bucket's own removal (#233). Call it
    /// with the pipeline locked.
    pub(crate) fn removal_of_uploads_into(&self, bucket: &str) -> S3Result<Vec<JournalRecord>> {
        let mut records = Vec::new();
        for open in self.uploads_into(bucket)? {
            if let Some(upload) = self.upload_at(&upload_namespace(&open.upload_id))? {
                records.extend(self.removal_of(&upload)?);
            }
        }
        Ok(records)
    }

    /// ListMultipartUploads (#234): the open uploads into `bucket` whose
    /// keys start with `prefix`, by key and then upload id, at most `max`
    /// of them. With `key_marker` the list starts after that key, or, with
    /// `upload_id_marker` as well, after that upload of that key. Without
    /// `key_marker`, `upload_id_marker` is ignored, as in S3.
    ///
    /// With a `delimiter`, uploads whose keys hold it past the prefix are
    /// rolled into one common prefix per key up to it, which counts toward
    /// `max` as an upload does, as ListObjects does (#253). A key marker
    /// that is a common prefix skips the uploads it covers, so the next
    /// page starts after it.
    pub fn list_multipart_uploads(
        &self,
        bucket: &str,
        prefix: &str,
        delimiter: Option<&str>,
        key_marker: Option<&str>,
        upload_id_marker: Option<&str>,
        max: usize,
    ) -> S3Result<UploadPage> {
        self.head_bucket(bucket)?;
        let after = |upload: &OpenUpload| match (key_marker, upload_id_marker) {
            (None, _) => true,
            (Some(key), None) => upload.key.as_str() > key,
            (Some(key), Some(id)) => (upload.key.as_str(), upload.upload_id.as_str()) > (key, id),
        };
        let mut page = UploadPage::default();
        // The common prefix listed last. Uploads are in key order, so those
        // of one common prefix come together.
        let mut last_group = key_marker.map(str::to_string);
        for upload in self.uploads_into(bucket)? {
            if !upload.key.starts_with(prefix) || !after(&upload) {
                continue;
            }
            let group = common_prefix(&upload.key, prefix.len(), delimiter);
            if group.is_some() && group == last_group {
                continue;
            }
            if page.uploads.len() + page.common_prefixes.len() == max {
                page.truncated = true;
                break;
            }
            match group {
                Some(group) => {
                    page.next_key_marker = Some(group.clone());
                    page.next_upload_id_marker = None;
                    page.common_prefixes.push(group.clone());
                    last_group = Some(group);
                }
                None => {
                    page.next_key_marker = Some(upload.key.clone());
                    page.next_upload_id_marker = Some(upload.upload_id.clone());
                    page.uploads.push(upload);
                }
            }
        }
        Ok(page)
    }

    /// The records that remove `upload`: its parts, dropping each capsule
    /// still there, its name and its namespace.
    fn removal_of(&self, upload: &Upload) -> S3Result<Vec<JournalRecord>> {
        let mut records = Vec::with_capacity(2 * upload.parts.len() + 2);
        for (number, id) in &upload.parts {
            records.push(JournalRecord::Unbind {
                ns: upload.ns.clone(),
                name: part_name(*number),
            });
            // A part `spacectl delete` removed has nothing left to drop.
            if self.registry.get_capsule(*id)?.is_some() {
                records.push(JournalRecord::Drop(*id));
            }
        }
        records.push(JournalRecord::Unbind {
            ns: upload.ns.clone(),
            name: String::new(),
        });
        records.push(JournalRecord::DropNamespace {
            ns: upload.ns.clone(),
            dropped_at: now_secs(),
        });
        Ok(records)
    }

    /// CreateMultipartUpload: start an upload of `key` in `bucket`, which
    /// gets `metadata`, as a PUT's headers, when it completes. Returns the
    /// upload id.
    pub async fn create_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        metadata: Vec<(String, String)>,
    ) -> S3Result<String> {
        check_key(key)?;
        check_metadata(&metadata)?;
        // A version 7 UUID starts with the time it was made, and the uuid
        // crate keeps those made in one process in order, so uploads of a
        // key list in the order they were started, as in S3 (#253).
        let upload_id = uuid::Uuid::now_v7().simple().to_string();
        let ns = upload_namespace(&upload_id);
        let mut fields = vec![
            (BUCKET_FIELD.to_string(), bucket.to_string()),
            (KEY_FIELD.to_string(), key.to_string()),
        ];
        fields.extend(metadata);
        let mut handle = self.pipeline.lock().await;
        // A store opened without its key takes no parts, so it starts no
        // uploads either (#157).
        handle.check_writable()?;
        self.head_bucket(bucket)?;
        handle.commit(vec![
            JournalRecord::Namespace {
                ns: ns.clone(),
                created_at: now_secs(),
            },
            JournalRecord::Bind {
                ns,
                name: String::new(),
                capsule_id: CapsuleId::from_uuid(uuid::Uuid::nil()),
                metadata: fields,
            },
        ])?;
        Ok(upload_id)
    }

    /// UploadPart: store part `number` of an upload, checked against the
    /// digests in `options` as a PUT body is. A part uploaded again
    /// replaces the one before, whose capsule is dropped. Returns the
    /// part's capsule, which is its ETag.
    pub async fn upload_part<S, E>(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        number: u32,
        data_stream: S,
        options: &PutOptions,
    ) -> S3Result<CapsuleId>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: Into<anyhow::Error>,
    {
        if !(1..=MAX_PART_NUMBER).contains(&number) {
            return Err(S3Error::InvalidArgument(format!(
                "Part number must be an integer between 1 and {MAX_PART_NUMBER}, inclusive"
            )));
        }
        self.head_bucket(bucket)?;
        // Fail before storing the body, and check again at the commit.
        self.upload(bucket, key, upload_id)?;
        let staged = self.stage_body(data_stream, options).await?;
        let capsule_id = staged.capsule_id();

        // Dropping `staged` on an error below aborts the write.
        let mut handle = self.pipeline.lock().await;
        let upload = self.upload(bucket, key, upload_id)?;
        let mut with = vec![JournalRecord::Bind {
            ns: upload.ns.clone(),
            name: part_name(number),
            capsule_id,
            metadata: Vec::new(),
        }];
        if let Some((_, previous)) = upload.parts.iter().find(|(n, _)| *n == number) {
            if self.registry.get_capsule(*previous)?.is_some() {
                with.push(JournalRecord::Drop(*previous));
            }
        }
        for capsule in staged.commit(&mut handle, with).await? {
            self.release(&mut handle, capsule).await;
        }
        Ok(capsule_id)
    }

    /// ListParts: the parts of an upload, by number.
    pub fn list_parts(&self, bucket: &str, key: &str, upload_id: &str) -> S3Result<Vec<Part>> {
        let upload = self.upload(bucket, key, upload_id)?;
        let mut parts = Vec::with_capacity(upload.parts.len());
        for (number, id) in upload.parts {
            if let Some(capsule) = self.registry.get_capsule(id)? {
                parts.push(Part {
                    number,
                    capsule_id: id,
                    size: capsule.size,
                    created_at: capsule.created_at,
                });
            }
        }
        Ok(parts)
    }

    /// AbortMultipartUpload: remove an upload and its parts, in one batch.
    pub async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> S3Result<()> {
        let mut handle = self.pipeline.lock().await;
        let upload = self.upload(bucket, key, upload_id)?;
        let records = self.removal_of(&upload)?;
        for capsule in handle.commit(records)? {
            self.release(&mut handle, capsule).await;
        }
        Ok(())
    }

    /// CompleteMultipartUpload: make the object from the parts `requested`
    /// names, as (number, ETag), in ascending order. Every part but the
    /// last must be at least [`MIN_PART_SIZE`]. Parts the upload holds but
    /// `requested` leaves out are dropped. Returns the object's capsule.
    ///
    /// The parts are read once, outside the pipeline lock, to compute the
    /// object's digest; each part's own digest is checked as it's read. A
    /// part uploaded again meanwhile fails the request with `InvalidPart`,
    /// and the client can complete again.
    pub async fn complete_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        requested: &[(u32, String)],
    ) -> S3Result<CapsuleId> {
        if requested.is_empty() {
            return Err(S3Error::MalformedXML(
                "a CompleteMultipartUpload names at least one Part".to_string(),
            ));
        }
        if !requested.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err(S3Error::InvalidPartOrder);
        }

        // Pick the parts, start reading them and lease them under the lock,
        // so nothing drops them before they're read.
        let (chosen, streams, leases) = {
            let handle = self.pipeline.lock().await;
            self.head_bucket(bucket)?;
            let upload = self.upload(bucket, key, upload_id)?;
            let mut chosen: Vec<(u32, Capsule)> = Vec::with_capacity(requested.len());
            for (i, (number, tag)) in requested.iter().enumerate() {
                let invalid = || {
                    S3Error::InvalidPart(format!(
                        "part {number} isn't in this upload with ETag {tag}"
                    ))
                };
                let id = upload
                    .parts
                    .iter()
                    .find(|(n, _)| n == number)
                    .map(|(_, id)| *id)
                    .ok_or_else(invalid)?;
                if tag.trim().trim_matches('"') != id.as_uuid().to_string() {
                    return Err(invalid());
                }
                let capsule = self.registry.get_capsule(id)?.ok_or_else(invalid)?;
                if i + 1 < requested.len() && capsule.size < MIN_PART_SIZE {
                    return Err(S3Error::EntityTooSmall(*number));
                }
                chosen.push((*number, capsule));
            }
            let mut streams = Vec::with_capacity(chosen.len());
            let mut leases = Vec::with_capacity(chosen.len());
            for (_, capsule) in &chosen {
                streams.push(handle.read_range_stream(capsule.id, 0, u64::MAX).await?);
                leases.push(self.lease(capsule.id));
            }
            (chosen, streams, leases)
        };

        let mut hasher = blake3::Hasher::new();
        for mut stream in streams {
            while let Some(chunk) = stream.next().await {
                hasher.update(&chunk?);
            }
        }
        let digest = *hasher.finalize().as_bytes();

        let mut handle = self.pipeline.lock().await;
        // Under the lock nothing else can commit, so the parts can't go
        // before this commit takes them.
        drop(leases);
        // DeleteBucket and AbortMultipartUpload hold this lock too.
        self.head_bucket(bucket)?;
        let upload = self.upload(bucket, key, upload_id)?;
        for (number, capsule) in &chosen {
            let same = upload
                .parts
                .iter()
                .any(|(n, id)| n == number && *id == capsule.id);
            if !same || self.registry.get_capsule(capsule.id)?.is_none() {
                return Err(S3Error::InvalidPart(format!(
                    "part {number} changed while the upload was being completed"
                )));
            }
        }
        let policy = chosen[0].1.policy.clone();
        let segments: Vec<SegmentId> = chosen
            .iter()
            .flat_map(|(_, capsule)| capsule.segments.iter().copied())
            .collect();
        let write = handle
            .begin_composed_write(&policy, &segments, digest)
            .await?;
        let capsule_id = write.capsule_id();
        let ns = namespace(bucket);
        let mut with = vec![JournalRecord::Bind {
            ns: ns.clone(),
            name: key.to_string(),
            capsule_id,
            metadata: upload.metadata.clone(),
        }];
        if let Some((previous, _)) = self.live_capsule_created(&ns, key)? {
            with.push(JournalRecord::Drop(previous));
        }
        with.extend(self.removal_of(&upload)?);
        for capsule in handle.finish_write_with(write, with).await? {
            self.release(&mut handle, capsule).await;
        }
        Ok(capsule_id)
    }

    /// Abort every upload started more than `age` ago, as a reclaim pass
    /// does with [`ABANDONED_UPLOAD_AGE`]. Returns how many it aborted.
    pub async fn abort_uploads_older_than(&self, age: Duration) -> S3Result<usize> {
        let cutoff = now_secs().saturating_sub(age.as_secs());
        let stale: Vec<String> = self
            .registry
            .list_namespaces(UPLOAD_PREFIX, None, usize::MAX)?
            .into_iter()
            .filter(|(_, created_at)| *created_at <= cutoff)
            .map(|(ns, _)| ns)
            .collect();
        let mut aborted = 0;
        for ns in stale {
            let mut handle = self.pipeline.lock().await;
            let records = match self.upload_at(&ns)? {
                Some(upload) => self.removal_of(&upload)?,
                // A namespace without its name, left by nothing this view
                // writes, still goes.
                None => vec![JournalRecord::DropNamespace {
                    ns,
                    dropped_at: now_secs(),
                }],
            };
            for capsule in handle.commit(records)? {
                self.release(&mut handle, capsule).await;
            }
            aborted += 1;
        }
        Ok(aborted)
    }
}
