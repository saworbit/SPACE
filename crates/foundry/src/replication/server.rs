//! Replication server (Replica node).
//!
//! This module implements the server-side of chain replication, running on
//! the replica node. It accepts connections from primary nodes and applies
//! writes to the local volume.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::Foundry;
use crate::backend::VolumeId;

use super::protocol::{ReplicationMessage, ReplicationResponse};

/// Start the replication server on the specified port.
///
/// The server accepts connections from primary nodes and applies writes
/// to local volumes identified by the handshake.
///
/// # Arguments
///
/// * `foundry` - Shared Foundry instance with access to volumes
/// * `port` - TCP port to listen on
///
/// # Returns
///
/// Never returns under normal operation. Errors are logged and connections
/// are dropped on protocol violations.
pub async fn start_replication_server(foundry: Arc<Foundry>, port: u16) -> crate::error::Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .map_err(|e| crate::error::FoundryError::config_error(format!("Failed to bind replication server: {}", e)))?;

    tracing::info!(port = port, "Replication server listening");

    loop {
        let (socket, addr) = listener
            .accept()
            .await
            .map_err(|e| crate::error::FoundryError::config_error(format!("Failed to accept connection: {}", e)))?;

        let foundry = foundry.clone();

        tracing::info!(peer = %addr, "Replication connection accepted");

        tokio::spawn(async move {
            if let Err(e) = handle_replication_connection(socket, foundry).await {
                tracing::error!(peer = %addr, error = %e, "Replication connection failed");
            }
        });
    }
}

/// Handle a single replication connection.
async fn handle_replication_connection(
    socket: tokio::net::TcpStream,
    foundry: Arc<Foundry>,
) -> crate::error::Result<()> {
    let (mut reader, mut writer) = socket.into_split();
    let mut active_volume = None;

    loop {
        // Read Length
        let len = match reader.read_u64().await {
            Ok(l) => l,
            Err(_) => break,
        };

        // Read Packet
        let mut buf = vec![0u8; len as usize];
        if reader.read_exact(&mut buf).await.is_err() {
            break;
        }

        let msg: ReplicationMessage = match bincode::deserialize(&buf) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "Failed to deserialize message");
                break;
            }
        };

        let response = match msg {
            ReplicationMessage::Handshake { volume_id } => {
                tracing::info!(volume_id = %volume_id, "Replication handshake");

                match volume_id.parse::<VolumeId>() {
                    Ok(vol_id) => match foundry.get_volume(vol_id).await {
                        Ok(vol) => {
                            active_volume = Some(vol);
                            ReplicationResponse::Ok
                        }
                        Err(e) => {
                            tracing::error!(volume_id = %volume_id, error = %e, "Volume not found");
                            ReplicationResponse::Error(format!("Volume not found: {}", e))
                        }
                    },
                    Err(e) => {
                        tracing::error!(volume_id = %volume_id, error = %e, "Invalid volume ID");
                        ReplicationResponse::Error(format!("Invalid Volume ID: {}", e))
                    }
                }
            }
            ReplicationMessage::Write { offset, data } => {
                if let Some(vol) = &active_volume {
                    // Apply Write Locally
                    match vol.write_at(offset, bytes::Bytes::from(data)).await {
                        Ok(_) => {
                            tracing::trace!(offset = offset, "Write replicated");
                            ReplicationResponse::Ok
                        }
                        Err(e) => {
                            tracing::error!(offset = offset, error = %e, "Write failed");
                            ReplicationResponse::Error(e.to_string())
                        }
                    }
                } else {
                    ReplicationResponse::Error("Handshake required".into())
                }
            }
            ReplicationMessage::Ack => {
                // Ack messages are sent by primary, not expected from replica
                ReplicationResponse::Ok
            }
        };

        // Send Ack
        let encoded = bincode::serialize(&response).unwrap();
        if writer.write_u64(encoded.len() as u64).await.is_err() {
            break;
        }
        if writer.write_all(&encoded).await.is_err() {
            break;
        }
    }

    Ok(())
}
