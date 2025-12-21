use crate::rpc::{
    federation_service_client::FederationServiceClient, CapsuleMetadata, HelloRequest, SegmentChunk,
};
use crate::zones::ZoneConfig;
use anyhow::{Context, Result};
use capsule_registry::CapsuleRegistry;
use common::{Capsule, CapsuleId, TransferPriority};
use nvram_sim::NvramLog;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::Request;
use tracing::{debug, info};

pub struct PeerClientManager {
    local_zone_id: String,
}

impl PeerClientManager {
    pub fn new(local_zone_id: impl Into<String>) -> Self {
        Self {
            local_zone_id: local_zone_id.into(),
        }
    }

    pub async fn connect(&self, zone: &ZoneConfig) -> Result<FederationServiceClient<Channel>> {
        let endpoint = zone.endpoint.clone();
        let client = FederationServiceClient::connect(endpoint)
            .await
            .with_context(|| format!("connect federation endpoint {}", zone.endpoint))?;
        Ok(client)
    }

    pub fn hello_request(&self, zone: &ZoneConfig) -> HelloRequest {
        HelloRequest {
            zone_id: self.local_zone_id.clone(),
            secret: zone.secret_key.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WanTransferAgent {
    pub chunk_bytes: usize,
    pub max_retries: usize,
}

impl Default for WanTransferAgent {
    fn default() -> Self {
        Self {
            chunk_bytes: 4 * 1024 * 1024,
            max_retries: 6,
        }
    }
}

impl WanTransferAgent {
    pub async fn replicate_capsule(
        &self,
        capsule_id: CapsuleId,
        source_registry: &CapsuleRegistry,
        source_nvram: &NvramLog,
        client_manager: &PeerClientManager,
        zone: &ZoneConfig,
        priority: TransferPriority,
    ) -> Result<()> {
        let capsule = source_registry
            .lookup(capsule_id)
            .with_context(|| format!("lookup capsule {}", capsule_id.as_uuid()))?;
        self.replicate_capsule_object(&capsule, source_nvram, client_manager, zone, priority)
            .await
    }

    pub async fn replicate_capsule_object(
        &self,
        capsule: &Capsule,
        source_nvram: &NvramLog,
        client_manager: &PeerClientManager,
        zone: &ZoneConfig,
        priority: TransferPriority,
    ) -> Result<()> {
        let started = Instant::now();
        let max_elapsed = Duration::from_secs(120);
        let mut delay = Duration::from_millis(200);
        let max_delay = Duration::from_secs(10);

        for attempt in 0..=self.max_retries {
            match self
                .replicate_capsule_attempt(
                    capsule,
                    source_nvram,
                    client_manager,
                    zone,
                    priority.clone(),
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let elapsed = started.elapsed();
                    let last_attempt = attempt == self.max_retries || elapsed >= max_elapsed;
                    if last_attempt {
                        return Err(err).context("federation replication failed after retries");
                    }

                    tracing::warn!(
                        error = %err,
                        capsule = %capsule.id.as_uuid(),
                        zone = %zone.name,
                        attempt,
                        "federation replication attempt failed; retrying"
                    );

                    sleep(delay).await;
                    delay = (delay + delay).min(max_delay);
                }
            }
        }

        anyhow::bail!("federation replication failed unexpectedly")
    }

    async fn replicate_capsule_attempt(
        &self,
        capsule: &Capsule,
        source_nvram: &NvramLog,
        client_manager: &PeerClientManager,
        zone: &ZoneConfig,
        priority: TransferPriority,
    ) -> Result<()> {
        let mut client = client_manager.connect(zone).await?;

        let mut hello_req = Request::new(client_manager.hello_request(zone));
        apply_secret(zone, &mut hello_req)?;
        let hello = client.hello(hello_req).await.context("hello")?.into_inner();
        if !hello.ok {
            anyhow::bail!("federation hello rejected: {}", hello.message);
        }

        let mut dest_segments: Vec<u64> = Vec::with_capacity(capsule.segments.len());

        for (idx, seg_id) in capsule.segments.iter().copied().enumerate() {
            let payload = source_nvram
                .read(seg_id)
                .with_context(|| format!("read segment {}", seg_id.0))?;
            let hash = blake3::hash(&payload);

            let chunk_size = self.chunk_bytes.max(1);
            let total_len = payload.len() as u64;
            let capsule_id_str = capsule.id.as_uuid().to_string();
            let hash_bytes: Vec<u8> = hash.as_bytes().to_vec();

            let chunks: Vec<SegmentChunk> = payload
                .chunks(chunk_size)
                .map(|chunk| SegmentChunk {
                    capsule_id: capsule_id_str.clone(),
                    segment_index: idx as u32,
                    content_hash: hash_bytes.clone(),
                    data: chunk.to_vec(),
                    total_len,
                })
                .collect();

            let stream = tokio_stream::iter(chunks);
            let mut req = Request::new(stream);
            apply_secret(zone, &mut req)?;

            let response = match priority {
                TransferPriority::Critical => client.push_segment(req).await,
                TransferPriority::Background => client.push_segment(req).await,
            }
            .with_context(|| format!("push_segment idx={idx}"))?
            .into_inner();

            if !response.ok {
                anyhow::bail!("segment transfer rejected: {}", response.message);
            }

            debug!(
                capsule = %capsule.id.as_uuid(),
                segment_index = idx,
                dest_segment_id = response.segment_id,
                "segment replicated"
            );
            dest_segments.push(response.segment_id);
        }

        let policy_json = serde_json::to_vec(&capsule.policy).context("serialize policy_json")?;
        let meta = CapsuleMetadata {
            capsule_id: capsule.id.as_uuid().to_string(),
            size: capsule.size,
            created_at: capsule.created_at,
            segment_ids: dest_segments,
            policy_json,
            deduped_bytes: capsule.deduped_bytes,
        };

        let mut register_req = Request::new(meta);
        apply_secret(zone, &mut register_req)?;
        let ack = client
            .register_capsule(register_req)
            .await
            .context("register_capsule")?
            .into_inner();

        if !ack.ok {
            anyhow::bail!("register_capsule rejected: {}", ack.message);
        }

        info!(
            capsule = %capsule.id.as_uuid(),
            zone = %zone.name,
            already_exists = ack.already_exists,
            "federation capsule registered"
        );

        Ok(())
    }
}

fn apply_secret<T>(zone: &ZoneConfig, request: &mut Request<T>) -> Result<()> {
    let secret = MetadataValue::try_from(zone.secret_key.clone())
        .context("encode x-space-secret metadata")?;
    request.metadata_mut().insert("x-space-secret", secret);
    Ok(())
}
