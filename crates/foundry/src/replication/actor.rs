//! Replication client (Primary node).
//!
//! This module implements the client-side of chain replication, running on
//! the primary node. It manages the connection to the replica and handles
//! write replication with stop-and-wait acknowledgement.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::error::{FoundryError, Result};

use super::protocol::{ReplicationMessage, ReplicationResponse};

/// Replication client for primary node.
///
/// Manages a persistent connection to a replica node and handles synchronous
/// write replication using a stop-and-wait protocol.
pub struct ReplicationClient {
    tx: mpsc::Sender<(ReplicationMessage, oneshot::Sender<Result<()>>)>,
}

impl ReplicationClient {
    /// Connect to a replica node and perform handshake.
    ///
    /// # Arguments
    ///
    /// * `target_addr` - Address of the replica node (e.g., "127.0.0.1:4421")
    /// * `volume_id` - Volume ID to replicate
    ///
    /// # Returns
    ///
    /// A `ReplicationClient` ready to replicate writes.
    pub async fn connect(target_addr: &str, volume_id: String) -> Result<Self> {
        let mut stream = TcpStream::connect(target_addr).await.map_err(|e| {
            FoundryError::config_error(format!("Failed to connect to replica: {}", e))
        })?;

        // 1. Handshake
        let handshake = ReplicationMessage::Handshake {
            volume_id: volume_id.clone(),
        };
        let encoded = bincode::serialize(&handshake)
            .map_err(|e| FoundryError::config_error(format!("Serialize error: {}", e)))?;

        // Write length-prefixed frame
        stream.write_u64(encoded.len() as u64).await.map_err(|e| {
            FoundryError::config_error(format!("Failed to write handshake length: {}", e))
        })?;
        stream
            .write_all(&encoded)
            .await
            .map_err(|e| FoundryError::config_error(format!("Failed to write handshake: {}", e)))?;

        // Wait for handshake ack
        let len = stream.read_u64().await.map_err(|e| {
            FoundryError::config_error(format!("Failed to read handshake response length: {}", e))
        })?;
        let mut buf = vec![0u8; len as usize];
        stream.read_exact(&mut buf).await.map_err(|e| {
            FoundryError::config_error(format!("Failed to read handshake response: {}", e))
        })?;

        let response: ReplicationResponse = bincode::deserialize(&buf)
            .map_err(|e| FoundryError::config_error(format!("Deserialize error: {}", e)))?;

        if let ReplicationResponse::Error(e) = response {
            return Err(FoundryError::config_error(format!(
                "Handshake failed: {}",
                e
            )));
        }

        tracing::info!(
            target = target_addr,
            volume_id = %volume_id,
            "Replication client connected"
        );

        // 2. Start Background Loop
        let (tx, mut rx) = mpsc::channel::<(ReplicationMessage, oneshot::Sender<Result<()>>)>(1024);
        let (mut reader, mut writer) = stream.into_split();

        tokio::spawn(async move {
            while let Some((msg, reply_tx)) = rx.recv().await {
                // Serialize
                let encoded = match bincode::serialize(&msg) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        let _ = reply_tx.send(Err(FoundryError::config_error(format!(
                            "Serialization failed: {}",
                            e
                        ))));
                        continue;
                    }
                };

                // Send
                if let Err(e) = writer.write_u64(encoded.len() as u64).await {
                    let _ = reply_tx.send(Err(FoundryError::config_error(format!(
                        "Failed to write message length: {}",
                        e
                    ))));
                    break;
                }
                if let Err(e) = writer.write_all(&encoded).await {
                    let _ = reply_tx.send(Err(FoundryError::config_error(format!(
                        "Failed to write message: {}",
                        e
                    ))));
                    break;
                }

                // Wait for Ack (Stop-and-Wait for simplicity in Phase 8.4)
                // Optimization: In Phase 9, we pipeline this using IDs.
                match reader.read_u64().await {
                    Ok(len) => {
                        let mut buf = vec![0u8; len as usize];
                        if reader.read_exact(&mut buf).await.is_ok() {
                            match bincode::deserialize::<ReplicationResponse>(&buf) {
                                Ok(ReplicationResponse::Ok) => {
                                    let _ = reply_tx.send(Ok(()));
                                }
                                Ok(ReplicationResponse::Error(e)) => {
                                    let _ = reply_tx.send(Err(FoundryError::config_error(
                                        format!("Replica error: {}", e),
                                    )));
                                }
                                Err(e) => {
                                    let _ = reply_tx.send(Err(FoundryError::config_error(
                                        format!("Failed to deserialize response: {}", e),
                                    )));
                                    break;
                                }
                            }
                        } else {
                            let _ = reply_tx.send(Err(FoundryError::config_error(
                                "Connection broken".to_string(),
                            )));
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = reply_tx.send(Err(FoundryError::config_error(format!(
                            "Failed to read response: {}",
                            e
                        ))));
                        break;
                    }
                }
            }
            tracing::warn!("Replication client loop terminated");
        });

        Ok(Self { tx })
    }

    /// Replicate a write to the secondary node.
    ///
    /// Returns only after the secondary has acknowledged persistence.
    ///
    /// # Arguments
    ///
    /// * `offset` - Byte offset in the volume
    /// * `data` - Data to write
    ///
    /// # Returns
    ///
    /// `Ok(())` if the replica successfully persisted the write.
    pub async fn replicate(&self, offset: u64, data: &[u8]) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();

        let msg = ReplicationMessage::Write {
            offset,
            data: data.to_vec(),
        };

        self.tx
            .send((msg, reply_tx))
            .await
            .map_err(|_| FoundryError::config_error("Replication actor dead"))?;

        reply_rx
            .await
            .map_err(|_| FoundryError::config_error("Replication response dropped"))?
    }
}
