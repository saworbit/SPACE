use anyhow::Context;
use bytes::Bytes;
use capsule_registry::modular_pipeline::{registry_pipeline_from_log, RegistryPipelineHandle};
use capsule_registry::CapsuleRegistry;
use common::{Capsule, CapsuleId};
use futures::{Stream, TryStreamExt};
use nvram_sim::NvramLog;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::io::{ReaderStream, StreamReader};
use tracing::warn;

pub mod handlers;
pub mod server;

/// Raw keys read from the store per batch while building a listing.
const LIST_BATCH: usize = 1000;

/// Errors the handlers turn into S3 error responses.
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("The specified bucket does not exist")]
    NoSuchBucket(String),
    #[error("The specified key does not exist")]
    NoSuchKey(String),
    #[error("The specified bucket is not valid")]
    InvalidBucketName(String),
    #[error("{0}")]
    InvalidArgument(String),
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
    pub content_type: String,
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

    fn new(key: &str, capsule: &Capsule) -> Self {
        Self {
            key: key.to_string(),
            capsule_id: capsule.id,
            size: capsule.size,
            created_at: capsule.created_at,
            content_type: detect_content_type(key).to_string(),
        }
    }
}

/// A GET result: the object's metadata and its bytes.
pub struct ObjectBody {
    pub meta: ObjectMapping,
    pub body: BodyStream,
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
    // Buckets and keys live in the registry's store, so they survive a
    // restart (#85).
    registry: CapsuleRegistry,
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
        }
    }

    /// Create a bucket. Creating one that exists is not an error.
    pub fn create_bucket(&self, bucket: &str) -> S3Result<()> {
        check_bucket_name(bucket)?;
        self.registry.create_bucket(bucket)?;
        Ok(())
    }

    pub fn head_bucket(&self, bucket: &str) -> S3Result<()> {
        match self.registry.bucket_created_at(bucket)? {
            Some(_) => Ok(()),
            None => Err(S3Error::NoSuchBucket(bucket.to_string())),
        }
    }

    /// PUT object. Creates the bucket if needed. Overwriting a key deletes
    /// the capsule it pointed at before (#86).
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
        check_bucket_name(bucket)?;
        let reader =
            StreamReader::new(data_stream.map_err(|err| std::io::Error::other(err.into())));
        tokio::pin!(reader);

        // BUFFERING BRIDGE:
        // The pipeline expects a slice; we isolate the buffering here until the pipeline accepts streams.
        let mut buffer = Vec::new();
        reader
            .read_to_end(&mut buffer)
            .await
            .context("failed to read streaming body into buffer bridge")?;

        self.registry.create_bucket(bucket)?;
        let mut handle = self.pipeline.lock().await;
        let policy = handle.default_policy();
        let capsule_id = handle.write_capsule(&buffer, &policy).await?;
        let previous = match self.registry.bind_object(bucket, key, capsule_id) {
            Ok(previous) => previous,
            Err(err) => {
                // No key points at the new capsule, so don't keep it.
                release_capsule(&mut handle, &self.registry, capsule_id).await;
                return Err(err.into());
            }
        };
        if let Some(old) = previous {
            release_capsule(&mut handle, &self.registry, old).await;
        }

        Ok(capsule_id)
    }

    /// GET object - read capsule data
    pub async fn get_object(&self, bucket: &str, key: &str) -> S3Result<ObjectBody> {
        // Hold the pipeline across the lookup and the read, so an overwrite
        // can't delete the capsule in between.
        let handle = self.pipeline.lock().await;
        let meta = self.head_object(bucket, key)?;
        let data = handle.read_capsule(meta.capsule_id).await?;
        drop(handle);

        Ok(ObjectBody {
            meta,
            body: Box::pin(ReaderStream::new(std::io::Cursor::new(data))),
        })
    }

    /// HEAD object - get metadata without reading data
    pub fn head_object(&self, bucket: &str, key: &str) -> S3Result<ObjectMapping> {
        let Some(id) = self.registry.object(bucket, key)? else {
            return Err(self.missing(bucket, key));
        };
        // A key whose capsule was removed with `spacectl delete` reads as
        // missing.
        match self.registry.get_capsule(id)? {
            Some(capsule) => Ok(ObjectMapping::new(key, &capsule)),
            None => Err(S3Error::NoSuchKey(key.to_string())),
        }
    }

    /// LIST objects in a bucket. Keys come back in byte order, without the
    /// bucket name. With a delimiter, keys that share a prefix up to it are
    /// rolled into one common prefix, which counts toward `max_keys`.
    pub fn list_objects(&self, bucket: &str, query: &ListQuery) -> S3Result<ListPage> {
        if self.registry.bucket_created_at(bucket)?.is_none() {
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
                    .list_objects(bucket, &query.prefix, after.as_deref(), LIST_BATCH)?;
            let full = batch.len() == LIST_BATCH;
            after = batch.last().map(|(key, _)| key.clone());

            for (key, id) in batch {
                let group = delimiter.and_then(|d| {
                    key[prefix_len..]
                        .find(d)
                        .map(|i| key[..prefix_len + i + d.len()].to_string())
                });
                if group.is_some() && group == last {
                    continue;
                }
                let object = match group {
                    Some(_) => None,
                    None => match self.registry.get_capsule(id)? {
                        Some(capsule) => Some(ObjectMapping::new(&key, &capsule)),
                        // Removed with `spacectl delete`.
                        None => continue,
                    },
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

    /// DELETE object. Deletes the capsule the key pointed at (#86).
    pub async fn delete_object(&self, bucket: &str, key: &str) -> S3Result<()> {
        let mut handle = self.pipeline.lock().await;
        match self.registry.unbind_object(bucket, key)? {
            Some(id) => {
                release_capsule(&mut handle, &self.registry, id).await;
                Ok(())
            }
            None => Err(self.missing(bucket, key)),
        }
    }

    fn missing(&self, bucket: &str, key: &str) -> S3Error {
        match self.registry.bucket_created_at(bucket) {
            Ok(None) => S3Error::NoSuchBucket(bucket.to_string()),
            _ => S3Error::NoSuchKey(key.to_string()),
        }
    }
}

/// Delete a capsule that no key points at any more. One already removed
/// with `spacectl delete` is skipped. A failure here only leaks space, so
/// it is logged instead of failing the request.
async fn release_capsule(
    handle: &mut RegistryPipelineHandle,
    registry: &CapsuleRegistry,
    id: CapsuleId,
) {
    if !matches!(registry.get_capsule(id), Ok(Some(_))) {
        return;
    }
    if let Err(err) = handle.delete_capsule(id).await {
        warn!(capsule = %id.as_uuid(), "failed to delete unreferenced capsule: {err:#}");
    }
}

/// A bucket name ends at the first '/' of a path, and the store separates
/// bucket from key with NUL, so a name can contain neither.
fn check_bucket_name(bucket: &str) -> S3Result<()> {
    if bucket.is_empty() || bucket.contains(['/', '\0']) {
        return Err(S3Error::InvalidBucketName(bucket.to_string()));
    }
    Ok(())
}

/// Simple content-type detection based on file extension.
///
/// Returns a static string to avoid per-call allocations in the hot path.
fn detect_content_type(key: &str) -> &'static str {
    if key.ends_with(".txt") {
        "text/plain"
    } else if key.ends_with(".json") {
        "application/json"
    } else if key.ends_with(".html") {
        "text/html"
    } else if key.ends_with(".jpg") || key.ends_with(".jpeg") {
        "image/jpeg"
    } else if key.ends_with(".png") {
        "image/png"
    } else if key.ends_with(".xml") {
        "application/xml"
    } else if key.ends_with(".css") {
        "text/css"
    } else if key.ends_with(".js") {
        "application/javascript"
    } else if key.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}
