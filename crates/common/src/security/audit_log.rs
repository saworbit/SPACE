use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use crate::Event;

/// Append-only audit log handle shared across components.
#[derive(Clone)]
pub struct AuditLog {
    inner: Arc<Mutex<AuditState>>,
    options: AuditOptions,
}

struct AuditState {
    file: File,
    last_hash: [u8; 32],
    events_since_flush: u32,
    events_since_tsa: u32,
    last_tsa: Option<TsaProof>,
}

#[derive(Clone)]
pub struct AuditOptions {
    pub path: PathBuf,
    pub flush_interval: u32,
    pub max_file_bytes: u64,
    pub tsa_batch_size: u32,
    pub tsa_client: Option<Arc<dyn TsaClient>>,
}

impl std::fmt::Debug for AuditOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditOptions")
            .field("path", &self.path)
            .field("flush_interval", &self.flush_interval)
            .field("max_file_bytes", &self.max_file_bytes)
            .field("tsa_batch_size", &self.tsa_batch_size)
            .field(
                "tsa_client",
                &self.tsa_client.as_ref().map(|_| "configured"),
            )
            .finish()
    }
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            path: PathBuf::from("space.audit.log"),
            flush_interval: 1,
            max_file_bytes: 1_024 * 1_024 * 1_024, // 1 GiB
            tsa_batch_size: 100,
            tsa_client: None,
        }
    }
}

/// Builder for configuring the audit log.
pub struct AuditLogBuilder {
    options: AuditOptions,
}

impl AuditLogBuilder {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            options: AuditOptions {
                path: path.into(),
                ..Default::default()
            },
        }
    }

    pub fn flush_interval(mut self, every: u32) -> Self {
        self.options.flush_interval = every.max(1);
        self
    }

    pub fn max_file_bytes(mut self, bytes: u64) -> Self {
        self.options.max_file_bytes = bytes;
        self
    }

    pub fn tsa_batch_size(mut self, batch: u32) -> Self {
        self.options.tsa_batch_size = batch.max(1);
        self
    }

    pub fn tsa_client(mut self, client: Arc<dyn TsaClient>) -> Self {
        self.options.tsa_client = Some(client);
        self
    }

    pub fn build(self) -> Result<AuditLog> {
        AuditLog::with_options(self.options)
    }
}

impl AuditLog {
    pub fn builder(path: impl Into<PathBuf>) -> AuditLogBuilder {
        AuditLogBuilder::new(path)
    }

    pub fn from_env() -> Result<Self> {
        let path = std::env::var("SPACE_AUDIT_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("space.audit.log"));
        let batch = std::env::var("SPACE_TSA_BATCH")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(100);
        let flush = std::env::var("SPACE_AUDIT_FLUSH")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);

        let tsa_client = std::env::var("SPACE_TSA_ENDPOINT").ok().map(|endpoint| {
            Arc::new(HttpTsaClient::new(
                endpoint,
                std::env::var("SPACE_TSA_API_KEY").ok(),
            )) as Arc<dyn TsaClient>
        });

        let builder = AuditLog::builder(path)
            .flush_interval(flush)
            .tsa_batch_size(batch);

        let builder = if let Some(client) = tsa_client {
            builder.tsa_client(client)
        } else {
            builder
        };

        builder.build()
    }

    fn with_options(options: AuditOptions) -> Result<Self> {
        if let Some(parent) = options.path.parent() {
            fs::create_dir_all(parent).ok();
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&options.path)
            .with_context(|| format!("unable to open audit log at {}", options.path.display()))?;

        let last_hash = recover_last_hash(&options.path)?;

        let state = AuditState {
            file,
            last_hash,
            events_since_flush: 0,
            events_since_tsa: 0,
            last_tsa: None,
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(state)),
            options,
        })
    }

    pub fn append(&self, event: Event) -> Result<AuditRecord> {
        let mut state = self.inner.lock().expect("audit mutex poisoned");
        let timestamp = unix_ts();
        let event_json = serde_json::to_string(&event)?;

        let mut hasher = Hasher::new();
        hasher.update(&state.last_hash);
        hasher.update(event_json.as_bytes());
        hasher.update(&timestamp.to_le_bytes());
        let digest = hasher.finalize();

        let mut next_hash = [0u8; 32];
        next_hash.copy_from_slice(digest.as_bytes());

        let mut record = AuditRecord {
            event,
            timestamp,
            prev_hash: hex::encode(state.last_hash),
            hash: hex::encode(next_hash),
            tsa_proof: None,
        };

        if let Some(client) = &self.options.tsa_client {
            state.events_since_tsa += 1;
            if state.events_since_tsa >= self.options.tsa_batch_size {
                let proof = client.timestamp(&record.hash)?;
                record.tsa_proof = Some(proof.clone());
                state.last_tsa = Some(proof);
                state.events_since_tsa = 0;
            }
        }

        write_record(&mut state.file, &record)?;
        state.last_hash = next_hash;

        state.events_since_flush += 1;
        if state.events_since_flush >= self.options.flush_interval {
            state.file.sync_data().ok();
            state.events_since_flush = 0;
        }

        if self.options.max_file_bytes > 0 {
            if let Ok(meta) = state.file.metadata() {
                if meta.len() >= self.options.max_file_bytes {
                    rotate_file(&self.options, &mut state)?;
                }
            }
        }

        Ok(record)
    }

    pub fn last_hash(&self) -> AuditTrail {
        let state = self.inner.lock().expect("audit mutex poisoned");
        AuditTrail {
            hash: hex::encode(state.last_hash),
            tsa: state.last_tsa.clone(),
        }
    }
}

fn write_record(file: &mut File, record: &AuditRecord) -> Result<()> {
    let line = serde_json::to_string(record)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn rotate_file(options: &AuditOptions, state: &mut AuditState) -> Result<()> {
    let file_name = options
        .path
        .file_name()
        .unwrap_or_else(|| OsStr::new("space.audit.log"))
        .to_string_lossy()
        .to_string();
    let rotated = options.path.with_file_name(format!("{file_name}.1"));

    state.file.sync_all().ok();
    drop(fs::rename(&options.path, &rotated));

    state.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&options.path)?;
    Ok(())
}

fn recover_last_hash(path: &Path) -> Result<[u8; 32]> {
    if !path.exists() {
        return Ok([0u8; 32]);
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut last = [0u8; 32];

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditRecord>(&line) {
            Ok(record) => {
                if let Ok(bytes) = hex::decode(record.hash.clone()) {
                    if bytes.len() == 32 {
                        last.copy_from_slice(&bytes);
                    }
                }
            }
            Err(err) => {
                warn!(error = %err, "failed to parse audit record");
            }
        }
    }

    Ok(last)
}

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub event: Event,
    pub timestamp: u64,
    pub prev_hash: String,
    pub hash: String,
    pub tsa_proof: Option<TsaProof>,
}

#[derive(Debug, Clone)]
pub struct AuditTrail {
    pub hash: String,
    pub tsa: Option<TsaProof>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsaProof {
    pub authority: String,
    pub timestamp: u64,
    pub token: String,
}

pub trait TsaClient: Send + Sync {
    fn timestamp(&self, digest_hex: &str) -> Result<TsaProof>;
}

/// Simple HTTP TSA client that POSTs digests to an external service.
pub struct HttpTsaClient {
    endpoint: String,
    api_key: Option<String>,
    client: reqwest::blocking::Client,
}

impl HttpTsaClient {
    pub fn new(endpoint: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl TsaClient for HttpTsaClient {
    fn timestamp(&self, digest_hex: &str) -> Result<TsaProof> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .json(&json!({ "digest": digest_hex }));
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }

        let response = request.send().context("tsa request failed")?;
        if !response.status().is_success() {
            return Err(anyhow!("tsa responded with {}", response.status()));
        }
        let body: serde_json::Value = response.json().context("tsa payload invalid")?;

        Ok(TsaProof {
            authority: body
                .get("authority")
                .and_then(|v| v.as_str())
                .unwrap_or("tsa")
                .to_string(),
            timestamp: body
                .get("timestamp")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(unix_ts),
            token: body
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_updates_hash_chain() {
        let path = std::env::temp_dir().join(format!(
            "space-audit-{}.log",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let log = AuditLog::builder(&path).build().unwrap();

        let event = Event::AuditHeartbeat {
            timestamp: unix_ts(),
            capsules: 1,
            segments: 1,
        };

        let record = log.append(event).unwrap();
        assert!(!record.hash.is_empty());
        let _ = std::fs::remove_file(path);
    }
}
