use bytes::Bytes;
use capsule_registry::modular_pipeline::{
    self, registry_pipeline_from_log, stage_from_reader, ReclaimReport, RegistryPipelineHandle,
};
use capsule_registry::CapsuleRegistry;
use common::format::JournalRecord;
use common::scrub::{ScrubConfig, ScrubState};
use common::{Capsule, CapsuleId};
use futures::{Stream, StreamExt, TryStreamExt};
use nvram_sim::NvramLog;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::io::StreamReader;
use tracing::warn;

pub mod aws_chunked;
pub mod checksum;
pub mod handlers;
pub mod server;

/// Raw keys read from the store per batch while building a listing.
const LIST_BATCH: usize = 1000;

/// The longest string the capsule journal can record: its strings carry a
/// 16-bit length (`docs/on-disk-format.md` §4.1).
const MAX_JOURNAL_STR: usize = u16::MAX as usize;

/// Each bucket is a namespace in the registry, holding its keys as names.
const NAMESPACE_PREFIX: &str = "s3:";

/// The headers a PUT stores with an object and GET and HEAD return, besides
/// `x-amz-meta-*` (#163). Names are lowercase.
pub const STORED_HEADERS: [&str; 6] = [
    "content-type",
    "content-disposition",
    "content-encoding",
    "content-language",
    "cache-control",
    "expires",
];

/// The prefix of a user metadata header.
pub const USER_METADATA_PREFIX: &str = "x-amz-meta-";

/// S3's limit on user metadata: the bytes of each `x-amz-meta-*` name,
/// without the prefix, and of each value, added up.
const MAX_USER_METADATA: usize = 2048;

fn namespace(bucket: &str) -> String {
    format!("{NAMESPACE_PREFIX}{bucket}")
}

/// Errors the handlers turn into S3 error responses.
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("The specified bucket does not exist")]
    NoSuchBucket(String),
    #[error("The bucket you tried to delete is not empty")]
    BucketNotEmpty(String),
    /// The body doesn't have the digest a header gave for it (#196).
    #[error("{}", bad_digest_message(.0))]
    BadDigest(checksum::Expected),
    /// A request from a host or web page the server doesn't answer (#200).
    #[error("Access Denied: {0}")]
    AccessDenied(String),
    #[error(
        "Your metadata headers exceed the maximum allowed metadata size: the names and \
         values of the x-amz-meta-* headers can add up to at most 2048 bytes"
    )]
    MetadataTooLarge,
    #[error("The specified key does not exist")]
    NoSuchKey(String),
    #[error(
        "The specified bucket is not valid. A bucket name is 3 to 63 lowercase letters, \
         digits, dots and hyphens. It starts and ends with a letter or digit, has no two \
         dots in a row, and isn't an IP address."
    )]
    InvalidBucketName(String),
    #[error("{0}")]
    InvalidArgument(String),
    /// A request that's malformed in a way no other error names, such as
    /// an `aws-chunked` body whose framing is wrong (#178).
    #[error("{0}")]
    InvalidRequest(String),
    /// A `Content-MD5` that isn't an MD5 digest in base64.
    #[error("{0}")]
    InvalidDigest(String),
    /// A request body that isn't the XML document the operation takes.
    #[error("The XML you provided was not well-formed or did not validate: {0}")]
    MalformedXML(String),
    /// Carries the object size for the `Content-Range` header.
    #[error("The requested range is not satisfiable")]
    InvalidRange(u64),
    /// An S3 operation, or an option of one, that the server doesn't do.
    #[error("{0}")]
    NotImplemented(String),
    /// An `If-Match`, `If-None-Match` on a PUT, or `If-Unmodified-Since`
    /// didn't hold (#171).
    #[error("At least one of the pre-conditions you specified did not hold")]
    PreconditionFailed,
    /// A GET or HEAD whose `If-None-Match` or `If-Modified-Since` says the
    /// client has this object already. Not a failure: it's answered with a
    /// 304 carrying the object's validators.
    #[error("Not Modified")]
    NotModified(Box<ObjectMapping>),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type S3Result<T> = std::result::Result<T, S3Error>;

/// Object bytes returned by GET.
pub type BodyStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

/// An S3 key and the capsule it points at.
#[derive(Debug, Clone)]
pub struct ObjectMapping {
    pub key: String,
    pub capsule_id: CapsuleId,
    pub size: u64,
    pub created_at: u64,
    /// The type stored with the object, or one guessed from the key's
    /// extension.
    pub content_type: String,
    /// The other headers stored with the object (#163).
    pub metadata: Vec<(String, String)>,
}

impl ObjectMapping {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn capsule_id(&self) -> CapsuleId {
        self.capsule_id
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn new(key: &str, capsule: &Capsule, mut metadata: Vec<(String, String)>) -> Self {
        let content_type = match metadata.iter().position(|(name, _)| name == "content-type") {
            Some(i) => metadata.remove(i).1,
            None => detect_content_type(key).to_string(),
        };
        Self {
            key: key.to_string(),
            capsule_id: capsule.id,
            size: capsule.size,
            created_at: capsule.created_at,
            content_type,
            metadata,
        }
    }
}

/// A GET result: the object's metadata and its bytes.
pub struct ObjectBody {
    pub meta: ObjectMapping,
    /// The first and last byte sent, when only part of the object is.
    pub range: Option<(u64, u64)>,
    pub body: BodyStream,
}

/// A `Range: bytes=` request, before it is matched against the object size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=first-` or `bytes=first-last`
    From { first: u64, last: Option<u64> },
    /// `bytes=-n`, the last n bytes
    Suffix(u64),
}

impl ByteRange {
    /// Parse a `Range` header. Anything but one byte range is `None`, and the
    /// whole object is sent, which RFC 9110 allows.
    pub fn parse(header: &str) -> Option<Self> {
        let spec = header.trim().strip_prefix("bytes=")?;
        if spec.contains(',') {
            return None;
        }
        let (first, last) = spec.split_once('-')?;
        let (first, last) = (first.trim(), last.trim());
        if first.is_empty() {
            return last.parse().ok().map(ByteRange::Suffix);
        }
        let first = first.parse().ok()?;
        let last = match last {
            "" => None,
            last => Some(last.parse().ok()?),
        };
        if matches!(last, Some(last) if last < first) {
            return None;
        }
        Some(ByteRange::From { first, last })
    }

    /// The first and last byte this range covers in an object of `size`
    /// bytes, or `None` if it covers none.
    pub fn resolve(self, size: u64) -> Option<(u64, u64)> {
        match self {
            ByteRange::From { first, last } => {
                (first < size).then(|| (first, last.map_or(size - 1, |last| last.min(size - 1))))
            }
            ByteRange::Suffix(n) => (n > 0 && size > 0).then(|| (size - n.min(size), size - 1)),
        }
    }
}

/// What a PUT asks for besides its body.
#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    /// Checked against the object the key holds when the PUT commits (#171).
    pub conditions: Preconditions,
    /// Headers stored with the object, which GET and HEAD return (#163):
    /// any of [`STORED_HEADERS`], and `x-amz-meta-*`. Names are lowercase.
    pub metadata: Vec<(String, String)>,
    /// Digests the body must have (#196). They're computed as it streams
    /// in, and a body without them stores nothing.
    pub digests: Vec<checksum::Expected>,
    /// Set when the body is in `aws-chunked` encoding (#178).
    pub chunked: Option<ChunkedBody>,
}

/// A body in `aws-chunked` encoding (#178). It's decoded as it streams in,
/// and the object is the decoded bytes.
#[derive(Debug, Clone, Default)]
pub struct ChunkedBody {
    /// `x-amz-decoded-content-length`: the decoded body must be this long.
    pub decoded_length: Option<u64>,
    /// The checksums `x-amz-trailer` says the body's trailer carries. The
    /// body must have them.
    pub trailer: Vec<checksum::Algorithm>,
}

/// The digests an `aws-chunked` body's trailer gives. Each checksum in it
/// must be one `x-amz-trailer` named, and each one named must be in it.
fn trailer_digests(
    named: &[checksum::Algorithm],
    trailers: &[(String, String)],
) -> S3Result<Vec<checksum::Expected>> {
    let mut digests = Vec::new();
    for (name, value) in trailers {
        let Some(algorithm) = name.strip_prefix("x-amz-checksum-") else {
            // Such as x-amz-trailer-signature, which isn't checked.
            continue;
        };
        let algorithm = checksum::Algorithm::from_checksum_name(algorithm).ok_or_else(|| {
            S3Error::NotImplemented(format!("the {name} checksum is not implemented"))
        })?;
        if !named.contains(&algorithm) {
            return Err(S3Error::InvalidRequest(format!(
                "the body's trailer has {name}, which x-amz-trailer doesn't name"
            )));
        }
        digests.push(
            checksum::Expected::from_base64(name, algorithm, value).ok_or_else(|| {
                S3Error::InvalidArgument(format!("Value for {name} trailer is invalid."))
            })?,
        );
    }
    if let Some(missing) = named
        .iter()
        .find(|a| !digests.iter().any(|d| d.algorithm == **a))
    {
        return Err(S3Error::InvalidRequest(format!(
            "x-amz-trailer names a {} checksum, but the body's trailer doesn't have one",
            missing.name()
        )));
    }
    Ok(digests)
}

fn bad_digest_message(expected: &checksum::Expected) -> String {
    match (expected.header.as_str(), expected.algorithm) {
        ("x-amz-content-sha256", _) => {
            "The provided 'x-amz-content-sha256' header does not match what was computed."
                .to_string()
        }
        (_, checksum::Algorithm::Md5) => {
            "The Content-MD5 you specified did not match what we received.".to_string()
        }
        (_, algorithm) => format!(
            "The {} you specified did not match the calculated checksum.",
            algorithm.name()
        ),
    }
}

/// The conditional headers of a request (RFC 9110 §13), compared with the
/// object's ETag, its capsule id, and its Last-Modified, when the capsule
/// was created (#171).
#[derive(Debug, Clone, Default)]
pub struct Preconditions {
    /// `If-Match`: a list of ETags, or `*`.
    pub if_match: Option<String>,
    /// `If-None-Match`: a list of ETags, or `*`.
    pub if_none_match: Option<String>,
    /// `If-Modified-Since`, in seconds since the Unix epoch.
    pub if_modified_since: Option<u64>,
    /// `If-Unmodified-Since`, in seconds since the Unix epoch.
    pub if_unmodified_since: Option<u64>,
}

impl Preconditions {
    /// Check a GET or HEAD, in RFC 9110's order: `If-Match`, or else
    /// `If-Unmodified-Since`, fails the request; then `If-None-Match`, or
    /// else `If-Modified-Since`, answers it with 304.
    pub fn check_read(&self, meta: &ObjectMapping) -> S3Result<()> {
        if let Some(tags) = &self.if_match {
            if !etag_listed(tags, meta.capsule_id, false) {
                return Err(S3Error::PreconditionFailed);
            }
        } else if self
            .if_unmodified_since
            .is_some_and(|t| meta.created_at > t)
        {
            return Err(S3Error::PreconditionFailed);
        }
        let not_modified = match &self.if_none_match {
            Some(tags) => etag_listed(tags, meta.capsule_id, true),
            None => self.if_modified_since.is_some_and(|t| meta.created_at <= t),
        };
        if not_modified {
            return Err(S3Error::NotModified(Box::new(meta.clone())));
        }
        Ok(())
    }

    /// Check a PUT against the object the key holds now, if any: its id and
    /// when it was created. `If-None-Match: *` is create-only, and
    /// `If-Match` is compare-and-swap.
    fn check_write(&self, current: Option<(CapsuleId, u64)>) -> S3Result<()> {
        let holds = |tags: &str, weak| current.is_some_and(|(id, _)| etag_listed(tags, id, weak));
        let fails = self
            .if_match
            .as_deref()
            .is_some_and(|tags| !holds(tags, false))
            || self
                .if_none_match
                .as_deref()
                .is_some_and(|tags| holds(tags, true))
            || matches!(
                (current, self.if_unmodified_since),
                (Some((_, created_at)), Some(t)) if created_at > t
            );
        if fails {
            return Err(S3Error::PreconditionFailed);
        }
        Ok(())
    }
}

/// Whether a list of entity tags names this capsule's ETag. `*` names any
/// object. A weak tag (`W/"..."`) matches only in the weak comparison
/// `If-None-Match` uses. Quotes are optional, since some clients drop them.
fn etag_listed(tags: &str, id: CapsuleId, weak: bool) -> bool {
    let id = id.as_uuid().to_string();
    tags.split(',').map(str::trim).any(|tag| {
        let tag = match tag.strip_prefix("W/") {
            Some(_) if !weak => return false,
            Some(tag) => tag,
            None => tag,
        };
        tag == "*" || tag.trim_matches('"').eq_ignore_ascii_case(&id)
    })
}

/// Listing options, as in the ListObjectsV2 query.
#[derive(Debug, Clone)]
pub struct ListQuery {
    pub prefix: String,
    pub delimiter: Option<String>,
    pub start_after: Option<String>,
    pub max_keys: usize,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            delimiter: None,
            start_after: None,
            max_keys: 1000,
        }
    }
}

/// One page of a listing.
#[derive(Debug, Default)]
pub struct ListPage {
    pub objects: Vec<ObjectMapping>,
    pub common_prefixes: Vec<String>,
    /// Set when the page was cut at `max_keys`. The next page starts after it.
    pub next_start_after: Option<String>,
}

/// S3 Protocol View - provides S3-compatible access to capsules
pub struct S3View {
    pipeline: Arc<TokioMutex<RegistryPipelineHandle>>,
    // Buckets and keys are names in the registry, recorded in the capsule
    // journal, so they survive a restart (#85) and a lost space.db. Changes
    // to them are made with the pipeline locked.
    registry: CapsuleRegistry,
    leases: Arc<Mutex<HashMap<CapsuleId, LeaseState>>>,
}

/// GETs stream outside the pipeline lock. An overwrite or DELETE commits at
/// once, but a capsule it drops while a GET reads it keeps its segments
/// until the last reader is done, so the download isn't cut short. The
/// reader pins them (`docs/on-disk-format.md` §6).
#[derive(Default)]
struct LeaseState {
    readers: usize,
    /// The capsule, once it has been dropped.
    dropped: Option<Capsule>,
}

struct Lease {
    id: CapsuleId,
    leases: Arc<Mutex<HashMap<CapsuleId, LeaseState>>>,
    pipeline: Arc<TokioMutex<RegistryPipelineHandle>>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let dropped = {
            let mut leases = self.leases.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = leases.get_mut(&self.id) else {
                return;
            };
            state.readers -= 1;
            if state.readers > 0 {
                return;
            }
            leases.remove(&self.id).and_then(|state| state.dropped)
        };
        let Some(capsule) = dropped else {
            return;
        };
        let pipeline = Arc::clone(&self.pipeline);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    release_segments(&mut *pipeline.lock().await, &capsule).await;
                });
            }
            Err(_) => warn!(
                capsule = %self.id.as_uuid(),
                "segments of a dropped capsule stay until restart: no runtime to release them"
            ),
        }
    }
}

impl S3View {
    /// Open a view over an NVRAM log. This builds the same registry pipeline
    /// as `serve-s3`, so it fails when `SPACE_MASTER_KEY` is set but invalid.
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> anyhow::Result<Self> {
        Ok(Self::new_modular(registry_pipeline_from_log(
            nvram, registry,
        )?))
    }

    pub fn new_modular(handle: RegistryPipelineHandle) -> Self {
        let registry = handle.registry().clone();
        Self {
            pipeline: Arc::new(TokioMutex::new(handle)),
            registry,
            leases: Arc::default(),
        }
    }

    /// Create a bucket. Creating one that exists is not an error. A new
    /// bucket's name must follow S3's naming rules.
    pub fn create_bucket(&self, bucket: &str) -> S3Result<()> {
        let ns = namespace(bucket);
        if self.registry.namespace_created_at(&ns)?.is_none() {
            check_new_bucket_name(bucket)?;
            self.registry.create_namespace(&ns)?;
        }
        Ok(())
    }

    pub fn head_bucket(&self, bucket: &str) -> S3Result<()> {
        match self.registry.namespace_created_at(&namespace(bucket))? {
            Some(_) => Ok(()),
            None => Err(S3Error::NoSuchBucket(bucket.to_string())),
        }
    }

    /// Buckets whose names start with `prefix` and sort after
    /// `start_after`, in byte order, at most `limit` of them, with when
    /// each was created (#159).
    pub fn list_buckets(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> S3Result<Vec<(String, u64)>> {
        let after = start_after.map(namespace);
        Ok(self
            .registry
            .list_namespaces(&namespace(prefix), after.as_deref(), limit)?
            .into_iter()
            .map(|(ns, created_at)| (ns[NAMESPACE_PREFIX.len()..].to_string(), created_at))
            .collect())
    }

    /// DELETE a bucket (#159). As in S3, it must hold no objects, or it's
    /// `BucketNotEmpty`. Keys whose capsule `spacectl delete` removed read
    /// as missing, so they don't count: they're unbound in the batch that
    /// deletes the bucket. This runs under the pipeline lock, which a PUT
    /// holds to commit, so no key can be bound between the check and the
    /// delete.
    pub async fn delete_bucket(&self, bucket: &str) -> S3Result<()> {
        let mut handle = self.pipeline.lock().await;
        self.head_bucket(bucket)?;
        let ns = namespace(bucket);
        let mut records = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let batch = self
                .registry
                .list_names(&ns, "", after.as_deref(), LIST_BATCH)?;
            let full = batch.len() == LIST_BATCH;
            after = batch.last().map(|(name, _)| name.clone());
            for (name, binding) in batch {
                if self.registry.get_capsule(binding.capsule_id)?.is_some() {
                    return Err(S3Error::BucketNotEmpty(bucket.to_string()));
                }
                records.push(JournalRecord::Unbind {
                    ns: ns.clone(),
                    name,
                });
            }
            if !full {
                break;
            }
        }
        records.push(JournalRecord::DropNamespace {
            ns,
            dropped_at: now_secs(),
        });
        handle.commit(records)?;
        Ok(())
    }

    /// PUT object. As in S3, the bucket must exist (#164), so a typo in its
    /// name is `NoSuchBucket` rather than a new bucket. Overwriting a key
    /// deletes the capsule it pointed at before (#86). The capsule, its key
    /// and the drop of the capsule replaced commit as one batch
    /// (`docs/on-disk-format.md` §6), so a crash can't leave the new
    /// capsule without its key or the old one alive without one.
    pub async fn put_object<S, E>(
        &self,
        bucket: &str,
        key: &str,
        data_stream: S,
    ) -> S3Result<CapsuleId>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: Into<anyhow::Error>,
    {
        self.put_object_with(bucket, key, data_stream, &PutOptions::default())
            .await
    }

    /// [`S3View::put_object`], storing `options.metadata` with the object,
    /// if `options.conditions` hold for the object the key holds when the
    /// PUT commits. They're checked under the same lock as the commit, so
    /// two create-only or compare-and-swap PUTs can't both win (#171). A
    /// PUT that fails them, or whose metadata is refused, stores nothing.
    /// The metadata is in the same journal record as the key, so a crash
    /// can't leave one without the other.
    pub async fn put_object_with<S, E>(
        &self,
        bucket: &str,
        key: &str,
        data_stream: S,
        options: &PutOptions,
    ) -> S3Result<CapsuleId>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: Into<anyhow::Error>,
    {
        let ns = namespace(bucket);
        // Check the bucket before storing the body, and again at the commit.
        self.head_bucket(bucket)?;
        check_key(key)?;
        check_metadata(&options.metadata)?;
        let conditions = &options.conditions;
        // Fail early when the conditions already fail, rather than store the
        // body first. They're checked again at the commit.
        conditions.check_write(self.live_capsule_created(&ns, key)?)?;
        let algorithms = options.digests.iter().map(|d| d.algorithm).chain(
            options
                .chunked
                .iter()
                .flat_map(|c| c.trailer.iter().copied()),
        );
        let running = Arc::new(Mutex::new(checksum::Running::new(algorithms)));
        let digests = Arc::clone(&running);
        let body = data_stream.map_err(|err| std::io::Error::other(err.into()));
        // An aws-chunked body is decoded as it arrives (#178); the object,
        // and every digest, is the decoded bytes.
        let outcome = Arc::new(Mutex::new(aws_chunked::Outcome::default()));
        let decoded = match &options.chunked {
            Some(chunked) => futures::future::Either::Left(aws_chunked::decode(
                Box::pin(StreamReader::new(body)),
                chunked.decoded_length,
                Arc::clone(&outcome),
            )),
            None => futures::future::Either::Right(body),
        };
        let reader = StreamReader::new(decoded.inspect_ok(move |chunk| {
            digests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .update(chunk)
        }));
        // The body is stored one segment at a time as it arrives (#99).
        let policy = self.pipeline.lock().await.default_policy();
        let staged = match stage_from_reader(&self.pipeline, reader, &policy).await {
            Ok(staged) => staged,
            Err(err) => {
                let bad = outcome
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .error
                    .take();
                return Err(bad.map_or_else(|| err.into(), S3Error::InvalidRequest));
            }
        };
        let capsule_id = staged.capsule_id();
        // A body that doesn't have the digests its headers or its trailer
        // give stores nothing: its segments are released before the error
        // is returned.
        let mut expected = options.digests.clone();
        if let Some(chunked) = &options.chunked {
            let trailers =
                std::mem::take(&mut outcome.lock().unwrap_or_else(|e| e.into_inner()).trailers);
            match trailer_digests(&chunked.trailer, &trailers) {
                Ok(found) => expected.extend(found),
                Err(err) => {
                    staged.abort().await;
                    return Err(err);
                }
            }
        }
        let mismatch = running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .check(&expected);
        if let Some(expected) = mismatch {
            staged.abort().await;
            return Err(S3Error::BadDigest(expected));
        }

        // Dropping `staged` on an error below aborts the write.
        let mut handle = self.pipeline.lock().await;
        // DeleteBucket holds this lock, so a bucket that's here now is here
        // when the batch commits.
        self.head_bucket(bucket)?;
        let mut with = Vec::with_capacity(2);
        let current = self.live_capsule_created(&ns, key)?;
        conditions.check_write(current)?;
        let previous = current.map(|(id, _)| id);
        with.push(JournalRecord::Bind {
            ns,
            name: key.to_string(),
            capsule_id,
            metadata: options.metadata.clone(),
        });
        with.extend(previous.map(JournalRecord::Drop));
        for capsule in staged.commit(&mut handle, with).await? {
            self.release(&mut handle, capsule).await;
        }
        Ok(capsule_id)
    }

    /// GET object - read capsule data
    pub async fn get_object(&self, bucket: &str, key: &str) -> S3Result<ObjectBody> {
        self.get_object_range(bucket, key, None).await
    }

    /// GET an object, or the part of it `range` asks for. The body streams
    /// one segment at a time (#99).
    pub async fn get_object_range(
        &self,
        bucket: &str,
        key: &str,
        range: Option<ByteRange>,
    ) -> S3Result<ObjectBody> {
        self.get_object_if(bucket, key, range, &Preconditions::default())
            .await
    }

    /// [`S3View::get_object_range`], if `conditions` hold for the object
    /// read (#171). They're checked before the range.
    pub async fn get_object_if(
        &self,
        bucket: &str,
        key: &str,
        range: Option<ByteRange>,
        conditions: &Preconditions,
    ) -> S3Result<ObjectBody> {
        // Look up the key, start the read and take the lease under the
        // pipeline lock, so an overwrite can't delete the capsule first.
        let handle = self.pipeline.lock().await;
        let meta = self.head_object(bucket, key)?;
        conditions.check_read(&meta)?;
        let range = match range {
            Some(range) => Some(
                range
                    .resolve(meta.size)
                    .ok_or(S3Error::InvalidRange(meta.size))?,
            ),
            None => None,
        };
        let (offset, len) = range.map_or((0, meta.size), |(first, last)| (first, last - first + 1));
        let mut stream = handle
            .read_range_stream(meta.capsule_id, offset, len)
            .await?;
        let lease = self.lease(meta.capsule_id);
        drop(handle);

        // Decode the first segment before answering, so a read that fails
        // from the start, such as an encrypted object on a server without
        // its key (#155), is an error response rather than a 200 whose body
        // breaks off. A later segment can still fail mid-body.
        let first = match stream.next().await {
            Some(Err(err)) => return Err(err.into()),
            first => first,
        };
        let body = futures::stream::iter(first)
            .chain(stream)
            .map(move |chunk| {
                let _reading = &lease;
                chunk.map_err(std::io::Error::other)
            });
        Ok(ObjectBody {
            meta,
            range,
            body: Box::pin(body),
        })
    }

    /// HEAD object - get metadata without reading data
    pub fn head_object(&self, bucket: &str, key: &str) -> S3Result<ObjectMapping> {
        let Some(binding) = self.registry.binding(&namespace(bucket), key)? else {
            return Err(self.missing(bucket, key));
        };
        // A key whose capsule was removed with `spacectl delete` reads as
        // missing.
        match self.registry.get_capsule(binding.capsule_id)? {
            Some(capsule) => Ok(ObjectMapping::new(key, &capsule, binding.metadata)),
            None => Err(S3Error::NoSuchKey(key.to_string())),
        }
    }

    /// LIST objects in a bucket. Keys come back in byte order, without the
    /// bucket name. With a delimiter, keys that share a prefix up to it are
    /// rolled into one common prefix, which counts toward `max_keys`.
    pub fn list_objects(&self, bucket: &str, query: &ListQuery) -> S3Result<ListPage> {
        let ns = namespace(bucket);
        if self.registry.namespace_created_at(&ns)?.is_none() {
            return Err(S3Error::NoSuchBucket(bucket.to_string()));
        }

        let mut page = ListPage::default();
        if query.max_keys == 0 {
            return Ok(page);
        }
        let delimiter = query.delimiter.as_deref().filter(|d| !d.is_empty());
        let prefix_len = query.prefix.len();
        // The last key or common prefix returned. Starting from the previous
        // page's last common prefix skips the keys it already covered.
        let mut last = query.start_after.clone();
        let mut after = query.start_after.clone();

        loop {
            let batch =
                self.registry
                    .list_names(&ns, &query.prefix, after.as_deref(), LIST_BATCH)?;
            let full = batch.len() == LIST_BATCH;
            after = batch.last().map(|(key, _)| key.clone());

            for (key, binding) in batch {
                let group = delimiter.and_then(|d| {
                    key[prefix_len..]
                        .find(d)
                        .map(|i| key[..prefix_len + i + d.len()].to_string())
                });
                if group.is_some() && group == last {
                    continue;
                }
                // A key whose capsule an older `spacectl delete` removed
                // reads as missing, so it lists neither itself nor its
                // common prefix (#138).
                let Some(capsule) = self.registry.get_capsule(binding.capsule_id)? else {
                    continue;
                };
                let object = match group {
                    Some(_) => None,
                    None => Some(ObjectMapping::new(&key, &capsule, binding.metadata)),
                };
                if page.objects.len() + page.common_prefixes.len() == query.max_keys {
                    page.next_start_after = last;
                    return Ok(page);
                }
                if let Some(object) = object {
                    page.objects.push(object);
                    last = Some(key);
                } else if let Some(group) = group {
                    page.common_prefixes.push(group.clone());
                    last = Some(group);
                }
            }

            if !full {
                return Ok(page);
            }
        }
    }

    /// DELETE object. Deletes the capsule the key pointed at (#86), in the
    /// same batch that removes the key. As in S3, deleting a key that isn't
    /// there succeeds, so a retried DELETE does too (#160); a bucket that
    /// isn't there is still `NoSuchBucket`.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> S3Result<()> {
        let mut handle = self.pipeline.lock().await;
        let ns = namespace(bucket);
        let Some(id) = self.registry.name(&ns, key)? else {
            return match self.registry.namespace_created_at(&ns)? {
                Some(_) => Ok(()),
                None => Err(S3Error::NoSuchBucket(bucket.to_string())),
            };
        };
        // A capsule `spacectl delete` removed has nothing left to drop.
        let live = self.registry.get_capsule(id)?.is_some();
        let mut records = vec![JournalRecord::Unbind {
            ns,
            name: key.to_string(),
        }];
        records.extend(live.then_some(JournalRecord::Drop(id)));
        for capsule in handle.commit(records)? {
            self.release(&mut handle, capsule).await;
        }
        Ok(())
    }

    /// DeleteObjects: delete each of `keys` as [`S3View::delete_object`]
    /// does, in one batch, so a crash deletes all of them or none (#159).
    /// As with DELETE, a key that isn't there counts as deleted.
    pub async fn delete_objects(&self, bucket: &str, keys: &[String]) -> S3Result<()> {
        let mut handle = self.pipeline.lock().await;
        self.head_bucket(bucket)?;
        let ns = namespace(bucket);
        let mut records = Vec::new();
        // A key named twice is unbound twice, which changes nothing more,
        // and its capsule dropped once.
        let mut dropped = HashSet::new();
        for key in keys {
            let Some(id) = self.registry.name(&ns, key)? else {
                continue;
            };
            records.push(JournalRecord::Unbind {
                ns: ns.clone(),
                name: key.clone(),
            });
            // A capsule `spacectl delete` removed has nothing left to drop.
            if self.registry.get_capsule(id)?.is_some() && dropped.insert(id) {
                records.push(JournalRecord::Drop(id));
            }
        }
        for capsule in handle.commit(records)? {
            self.release(&mut handle, capsule).await;
        }
        Ok(())
    }

    /// Run one reclaim pass over the store: compact the data log once enough
    /// of it is reclaimable, or garbage-collect a file backend
    /// ([`modular_pipeline::reclaim`]). Segments a GET is still reading are
    /// kept.
    pub async fn reclaim(&self) -> S3Result<ReclaimReport> {
        Ok(modular_pipeline::reclaim(&self.pipeline).await?)
    }

    /// Scrub the store in the background on `config`'s schedule
    /// ([`RegistryPipelineHandle::spawn_scrub`]). The receiver reports each
    /// cycle; abort the handle to stop.
    pub async fn spawn_scrub(
        &self,
        config: ScrubConfig,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::watch::Receiver<ScrubState>,
    ) {
        modular_pipeline::spawn_scrub(&self.pipeline, config).await
    }

    /// The capsule a key points at and when it was created, unless
    /// `spacectl delete` removed it.
    fn live_capsule_created(&self, ns: &str, key: &str) -> S3Result<Option<(CapsuleId, u64)>> {
        match self.registry.name(ns, key)? {
            Some(id) => Ok(self
                .registry
                .get_capsule(id)?
                .map(|capsule| (id, capsule.created_at))),
            None => Ok(None),
        }
    }

    fn lease(&self, id: CapsuleId) -> Lease {
        let mut leases = self.leases.lock().unwrap_or_else(|e| e.into_inner());
        leases.entry(id).or_default().readers += 1;
        Lease {
            id,
            leases: Arc::clone(&self.leases),
            pipeline: Arc::clone(&self.pipeline),
        }
    }

    /// Release the segments of a capsule an overwrite or DELETE dropped,
    /// once no GET is reading it. Call with the pipeline locked.
    async fn release(&self, handle: &mut RegistryPipelineHandle, capsule: Capsule) {
        {
            let mut leases = self.leases.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = leases.get_mut(&capsule.id) {
                state.dropped = Some(capsule);
                return;
            }
        }
        release_segments(handle, &capsule).await;
    }

    fn missing(&self, bucket: &str, key: &str) -> S3Error {
        match self.registry.namespace_created_at(&namespace(bucket)) {
            Ok(None) => S3Error::NoSuchBucket(bucket.to_string()),
            _ => S3Error::NoSuchKey(key.to_string()),
        }
    }
}

/// Release a dropped capsule's segments. A failure here only keeps them
/// until the next restart recounts references, so it is logged instead of
/// failing the request.
async fn release_segments(handle: &mut RegistryPipelineHandle, capsule: &Capsule) {
    if let Err(err) = handle.release(capsule).await {
        warn!(capsule = %capsule.id.as_uuid(), "failed to release a dropped capsule's segments: {err:#}");
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A new bucket's name follows S3's rules (#164): 3 to 63 lowercase
/// letters, digits, dots and hyphens, starting and ending with a letter or
/// digit, with no two dots in a row, and not an IP address. So it can't
/// start with `-`, which keeps `/-/health` out of the bucket namespace.
/// Buckets created before these rules keep working; only new ones are
/// checked.
fn check_new_bucket_name(bucket: &str) -> S3Result<()> {
    let edge = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let ip = bucket.split('.').count() == 4
        && bucket
            .split('.')
            .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()));
    let valid = (3..=63).contains(&bucket.len())
        && bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && edge(bucket.bytes().next())
        && edge(bucket.bytes().last())
        && !bucket.contains("..")
        && !ip;
    if !valid {
        return Err(S3Error::InvalidBucketName(bucket.to_string()));
    }
    Ok(())
}

/// A key fits in the journal, and in a listing's XML. XML 1.0 can't carry
/// most control characters even escaped, so one key holding one would make
/// every listing of its bucket unparseable (#172).
fn check_key(key: &str) -> S3Result<()> {
    if key.len() > MAX_JOURNAL_STR {
        return Err(S3Error::InvalidArgument(format!(
            "keys can be at most {MAX_JOURNAL_STR} bytes"
        )));
    }
    let xml_illegal = |c: char| matches!(c, '\0'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}');
    if key.chars().any(xml_illegal) {
        return Err(S3Error::InvalidArgument(
            "keys can't hold control characters other than tab, newline and carriage return"
                .to_string(),
        ));
    }
    Ok(())
}

/// Metadata a PUT can store (#163): only [`STORED_HEADERS`] and
/// `x-amz-meta-*` with a lowercase name, each with a value a header can
/// carry, and user metadata within S3's 2 KB.
fn check_metadata(metadata: &[(String, String)]) -> S3Result<()> {
    let token =
        |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.!#$%&'*+^`|~".contains(&b);
    let mut user = 0;
    for (name, value) in metadata {
        match name.strip_prefix(USER_METADATA_PREFIX) {
            Some(rest) if !rest.is_empty() && rest.bytes().all(token) => {
                user += rest.len() + value.len();
            }
            _ if STORED_HEADERS.contains(&name.as_str()) => {}
            _ => {
                return Err(S3Error::InvalidArgument(format!(
                    "'{name}' can't be stored with an object"
                )))
            }
        }
        let header_value = |b: u8| b == b'\t' || (b' '..=b'~').contains(&b);
        if value.len() > MAX_JOURNAL_STR || !value.bytes().all(header_value) {
            return Err(S3Error::InvalidArgument(format!(
                "the {name} header must be ASCII and at most {MAX_JOURNAL_STR} bytes"
            )));
        }
    }
    if user > MAX_USER_METADATA {
        return Err(S3Error::MetadataTooLarge);
    }
    Ok(())
}

/// The content type of an object stored without one, from its key's
/// extension, ignoring case.
fn detect_content_type(key: &str) -> &'static str {
    const TYPES: &[(&str, &str)] = &[
        ("txt", "text/plain"),
        ("json", "application/json"),
        ("html", "text/html"),
        ("htm", "text/html"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("png", "image/png"),
        ("gif", "image/gif"),
        ("webp", "image/webp"),
        ("svg", "image/svg+xml"),
        ("xml", "application/xml"),
        ("css", "text/css"),
        ("js", "application/javascript"),
        ("csv", "text/csv"),
        ("md", "text/markdown"),
        ("pdf", "application/pdf"),
        ("mp4", "video/mp4"),
        ("webm", "video/webm"),
        ("mp3", "audio/mpeg"),
        ("wav", "audio/wav"),
        ("wasm", "application/wasm"),
        ("gz", "application/gzip"),
        ("zip", "application/zip"),
        ("tar", "application/x-tar"),
    ];
    let ext = key.rsplit_once('.').map_or("", |(_, ext)| ext);
    TYPES
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map_or("application/octet-stream", |(_, t)| t)
}
