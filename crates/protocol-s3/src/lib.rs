use anyhow::Result;
#[cfg(feature = "modular_pipeline")]
use capsule_registry::modular_pipeline::RegistryPipelineHandle;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::CapsuleId;
#[cfg(feature = "modular_pipeline")]
use common::Policy;
use nvram_sim::NvramLog;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
#[cfg(feature = "modular_pipeline")]
use tokio::sync::Mutex as TokioMutex;
use tokio::task;

pub mod handlers;
pub mod server;

/// Maps S3 keys to Capsule IDs
#[derive(Debug, Clone)]
pub struct KeyMapping {
    key: String,
    capsule_id: CapsuleId,
    size: u64,
    created_at: u64,
    content_type: String,
}

impl KeyMapping {
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
}

/// S3 Protocol View - provides S3-compatible access to capsules
enum PipelineBackend {
    Legacy(Arc<WritePipeline>),
    #[cfg(feature = "modular_pipeline")]
    Modular(Arc<TokioMutex<RegistryPipelineHandle>>),
}

pub struct S3View {
    pipeline: PipelineBackend,
    // Maps "bucket/key" -> CapsuleId
    key_map: Arc<RwLock<HashMap<String, KeyMapping>>>,
}

impl S3View {
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        Self {
            pipeline: PipelineBackend::Legacy(Arc::new(WritePipeline::new(registry, nvram))),
            key_map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[cfg(feature = "modular_pipeline")]
    pub fn new_modular(handle: RegistryPipelineHandle) -> Self {
        Self {
            pipeline: PipelineBackend::Modular(Arc::new(TokioMutex::new(handle))),
            key_map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// PUT object - create new capsule from data
    pub async fn put_object(&self, bucket: &str, key: &str, data: Vec<u8>) -> Result<CapsuleId> {
        let data_len = data.len();
        let capsule_id = match &self.pipeline {
            PipelineBackend::Legacy(pipeline) => {
                let pipeline = Arc::clone(pipeline);
                let payload = data.clone();
                task::spawn_blocking(move || pipeline.write_capsule(&payload))
                    .await
                    .map_err(|err| anyhow::anyhow!(err.to_string()))??
            }
            #[cfg(feature = "modular_pipeline")]
            PipelineBackend::Modular(pipeline) => {
                let mut handle = pipeline.lock().await;
                handle.write_capsule(&data, &Policy::default()).await?
            }
        };

        // Map S3 key to capsule
        let full_key = format!("{}/{}", bucket, key);
        let mapping = KeyMapping {
            key: full_key.clone(),
            capsule_id,
            size: data_len as u64,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            content_type: detect_content_type(key),
        };

        self.key_map.write().unwrap().insert(full_key, mapping);

        Ok(capsule_id)
    }

    /// GET object - read capsule data
    pub async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>> {
        let full_key = format!("{}/{}", bucket, key);

        let mapping = self
            .key_map
            .read()
            .unwrap()
            .get(&full_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Key not found: {}", full_key))?;

        match &self.pipeline {
            PipelineBackend::Legacy(pipeline) => {
                let pipeline = Arc::clone(pipeline);
                task::spawn_blocking(move || pipeline.read_capsule(mapping.capsule_id))
                    .await
                    .map_err(|err| anyhow::anyhow!(err.to_string()))?
            }
            #[cfg(feature = "modular_pipeline")]
            PipelineBackend::Modular(pipeline) => {
                let handle = pipeline.lock().await;
                handle.read_capsule(mapping.capsule_id).await
            }
        }
    }

    /// HEAD object - get metadata without reading data
    pub fn head_object(&self, bucket: &str, key: &str) -> Result<KeyMapping> {
        let full_key = format!("{}/{}", bucket, key);

        self.key_map
            .read()
            .unwrap()
            .get(&full_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Key not found: {}", full_key))
    }

    /// LIST objects in bucket
    pub fn list_objects(&self, bucket: &str) -> Result<Vec<KeyMapping>> {
        let prefix = format!("{}/", bucket);

        Ok(self
            .key_map
            .read()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v.clone())
            .collect())
    }

    /// DELETE object
    pub fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        let full_key = format!("{}/{}", bucket, key);

        self.key_map
            .write()
            .unwrap()
            .remove(&full_key)
            .ok_or_else(|| anyhow::anyhow!("Key not found: {}", full_key))?;

        // Note: We're not deleting the capsule itself yet - that's for Phase 3
        // For now, capsules are only deleted when explicitly removed via spacectl

        Ok(())
    }
}

/// Simple content-type detection based on file extension
fn detect_content_type(key: &str) -> String {
    if key.ends_with(".txt") {
        "text/plain".to_string()
    } else if key.ends_with(".json") {
        "application/json".to_string()
    } else if key.ends_with(".html") {
        "text/html".to_string()
    } else if key.ends_with(".jpg") || key.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else if key.ends_with(".png") {
        "image/png".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}
