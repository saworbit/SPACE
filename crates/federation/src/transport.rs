//! gRPC transport layer for Raft consensus messages.
//!
//! This module provides the network transport for Raft protocol messages,
//! replacing the in-process channels used in Phase 9.1.

use crate::rpc::raft_service_server::{RaftService, RaftServiceServer};
use crate::rpc::{RaftMessageRequest, RaftMessageResponse};
use anyhow::{Context, Result};
use prost::Message as ProstMessage;
use raft::prelude::Message;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};
use tracing::{debug, error};

/// Registry mapping Raft node IDs to their gRPC endpoints.
///
/// This allows looking up peer addresses by their numeric node ID,
/// enabling the transport client to route messages correctly.
#[derive(Clone)]
pub struct PeerRegistry {
    peers: Arc<RwLock<HashMap<u64, String>>>,
}

impl PeerRegistry {
    /// Create a new empty peer registry.
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a peer with its gRPC endpoint.
    ///
    /// # Arguments
    /// - `node_id`: The Raft node ID
    /// - `addr`: The gRPC endpoint (e.g., "http://127.0.0.1:4422")
    pub fn add_peer(&self, node_id: u64, addr: String) {
        debug!(node_id = node_id, addr = %addr, "registered peer");
        self.peers.write().unwrap().insert(node_id, addr);
    }

    /// Look up a peer's gRPC endpoint by node ID.
    pub fn get_peer(&self, node_id: u64) -> Option<String> {
        self.peers.read().unwrap().get(&node_id).cloned()
    }

    /// Create a registry from a list of (node_id, address) pairs.
    ///
    /// This is a convenience method for bulk initialization.
    ///
    /// # Example
    /// ```ignore
    /// let registry = PeerRegistry::from_config(&[
    ///     (1, "http://127.0.0.1:4422"),
    ///     (2, "http://127.0.0.1:4423"),
    ///     (3, "http://127.0.0.1:4424"),
    /// ]);
    /// ```
    pub fn from_config(peers: &[(u64, &str)]) -> Self {
        let registry = Self::new();
        let map: HashMap<u64, String> = peers
            .iter()
            .map(|(id, addr)| (*id, addr.to_string()))
            .collect();
        *registry.peers.write().unwrap() = map;
        registry
    }

    /// Remove a peer from the registry.
    pub fn remove_peer(&self, node_id: u64) {
        self.peers.write().unwrap().remove(&node_id);
        debug!(node_id = node_id, "removed peer");
    }
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// gRPC server implementation for receiving Raft messages.
///
/// This service receives Raft protocol messages from other nodes
/// and forwards them to the local RaftEngine's inbox channel.
pub struct RaftServiceImpl {
    inbox: mpsc::Sender<Message>,
}

impl RaftServiceImpl {
    /// Create a new RaftService implementation.
    ///
    /// # Arguments
    /// - `inbox`: The channel to send received messages to the RaftEngine
    pub fn new(inbox: mpsc::Sender<Message>) -> Self {
        Self { inbox }
    }
}

#[tonic::async_trait]
impl RaftService for RaftServiceImpl {
    async fn send_message(
        &self,
        request: Request<RaftMessageRequest>,
    ) -> Result<Response<RaftMessageResponse>, Status> {
        let req = request.into_inner();

        // Deserialize the Raft message from bytes
        let msg = Message::decode(&req.message[..]).map_err(|e| {
            error!(error = %e, "failed to decode raft message");
            Status::invalid_argument(format!("failed to decode raft message: {}", e))
        })?;

        debug!(
            from = msg.from,
            to = msg.to,
            msg_type = msg.msg_type,
            term = msg.term,
            "received raft message"
        );

        // Forward to RaftEngine's inbox
        self.inbox.send(msg).await.map_err(|_| {
            error!("raft engine inbox is closed");
            Status::internal("raft engine is unavailable")
        })?;

        Ok(Response::new(RaftMessageResponse {
            ok: true,
            error: String::new(),
        }))
    }
}

/// gRPC client for sending Raft messages to peers.
///
/// This client maintains a connection pool to efficiently send messages
/// to other nodes in the Raft cluster.
pub struct RaftTransportClient {
    registry: Arc<PeerRegistry>,
    /// Connection pool: node_id -> gRPC client
    connections: Arc<
        tokio::sync::RwLock<
            HashMap<u64, crate::rpc::raft_service_client::RaftServiceClient<Channel>>,
        >,
    >,
}

impl RaftTransportClient {
    /// Create a new transport client with the given peer registry.
    pub fn new(registry: Arc<PeerRegistry>) -> Self {
        Self {
            registry,
            connections: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Send a Raft message to the specified peer.
    ///
    /// This method handles connection pooling, serialization, and error handling.
    /// Network errors are logged but not fatal - Raft's retry logic will handle them.
    ///
    /// # Arguments
    /// - `to`: The target node ID
    /// - `msg`: The Raft message to send
    ///
    /// # Errors
    /// Returns an error if:
    /// - The peer is not in the registry
    /// - Serialization fails
    /// - The network request fails
    pub async fn send(&self, to: u64, msg: Message) -> Result<()> {
        debug!(
            from = msg.from,
            to = msg.to,
            msg_type = msg.msg_type,
            term = msg.term,
            "sending raft message"
        );

        // Get or create a gRPC client for this peer
        let mut client = self.get_or_connect(to).await?;

        // Serialize the message
        let mut buf = Vec::new();
        msg.encode(&mut buf)
            .context("failed to encode raft message")?;

        // Send the message
        let request = Request::new(RaftMessageRequest { message: buf });
        let response = client
            .send_message(request)
            .await
            .context("grpc request failed")?;

        let resp = response.into_inner();
        if !resp.ok {
            anyhow::bail!("peer rejected message: {}", resp.error);
        }

        Ok(())
    }

    /// Get an existing gRPC client or create a new connection.
    ///
    /// This implements connection pooling to avoid creating a new
    /// connection for every message.
    async fn get_or_connect(
        &self,
        node_id: u64,
    ) -> Result<crate::rpc::raft_service_client::RaftServiceClient<Channel>> {
        // Check if we already have a connection
        {
            let conns = self.connections.read().await;
            if let Some(client) = conns.get(&node_id) {
                return Ok(client.clone());
            }
        }

        // Look up the peer address
        let addr = self
            .registry
            .get_peer(node_id)
            .ok_or_else(|| anyhow::anyhow!("unknown peer: {}", node_id))?;

        debug!(node_id = node_id, addr = %addr, "connecting to peer");

        // Create a new connection
        let client = crate::rpc::raft_service_client::RaftServiceClient::connect(addr.clone())
            .await
            .context("failed to connect to peer")?;

        // Cache the connection
        self.connections
            .write()
            .await
            .insert(node_id, client.clone());

        Ok(client)
    }

    /// Remove a cached connection for a peer.
    ///
    /// This is useful when a connection fails and needs to be re-established.
    pub async fn disconnect(&self, node_id: u64) {
        self.connections.write().await.remove(&node_id);
        debug!(node_id = node_id, "disconnected from peer");
    }
}

/// Start a gRPC server for the RaftService.
///
/// This is a convenience function for testing and simple deployments.
/// Production deployments should use `serve_with_raft` from the server module
/// to run both FederationService and RaftService on the same port.
///
/// # Arguments
/// - `addr`: The address to bind to (e.g., "127.0.0.1:4422")
/// - `inbox`: The channel to send received messages to
///
/// # Example
/// ```ignore
/// let (inbox_tx, inbox_rx) = mpsc::channel(100);
/// tokio::spawn(start_raft_server("127.0.0.1:4422".parse()?, inbox_tx));
/// ```
pub async fn start_raft_server(
    addr: std::net::SocketAddr,
    inbox: mpsc::Sender<Message>,
) -> Result<()> {
    let service = RaftServiceImpl::new(inbox);

    tonic::transport::Server::builder()
        .add_service(RaftServiceServer::new(service))
        .serve(addr)
        .await
        .context("failed to serve raft grpc")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use raft::prelude::MessageType;
    use std::net::SocketAddr;
    use tokio::time::Duration;

    #[test]
    fn test_peer_registry() {
        let registry = PeerRegistry::new();

        registry.add_peer(1, "http://127.0.0.1:4422".to_string());
        registry.add_peer(2, "http://127.0.0.1:4423".to_string());

        assert_eq!(
            registry.get_peer(1),
            Some("http://127.0.0.1:4422".to_string())
        );
        assert_eq!(
            registry.get_peer(2),
            Some("http://127.0.0.1:4423".to_string())
        );
        assert_eq!(registry.get_peer(3), None);

        registry.remove_peer(1);
        assert_eq!(registry.get_peer(1), None);
    }

    #[test]
    fn test_peer_registry_from_config() {
        let registry = PeerRegistry::from_config(&[
            (1, "http://127.0.0.1:4422"),
            (2, "http://127.0.0.1:4423"),
            (3, "http://127.0.0.1:4424"),
        ]);

        assert_eq!(
            registry.get_peer(1),
            Some("http://127.0.0.1:4422".to_string())
        );
        assert_eq!(
            registry.get_peer(2),
            Some("http://127.0.0.1:4423".to_string())
        );
        assert_eq!(
            registry.get_peer(3),
            Some("http://127.0.0.1:4424".to_string())
        );
    }

    #[tokio::test]
    async fn test_raft_service_message_handling() {
        let (inbox_tx, mut inbox_rx) = mpsc::channel(10);
        let addr: SocketAddr = "127.0.0.1:50052".parse().unwrap();

        // Start server
        let service = RaftServiceImpl::new(inbox_tx);
        let server_handle = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(RaftServiceServer::new(service))
                .serve(addr)
                .await
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create client and send message
        let registry = PeerRegistry::new();
        registry.add_peer(2, format!("http://{}", addr));

        let client = RaftTransportClient::new(Arc::new(registry));

        let msg = Message {
            msg_type: MessageType::MsgHeartbeat as i32,
            from: 1,
            to: 2,
            term: 1,
            ..Default::default()
        };

        // Send message
        client.send(2, msg.clone()).await.unwrap();

        // Verify message received
        let received = tokio::time::timeout(Duration::from_secs(1), inbox_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received.from, 1);
        assert_eq!(received.to, 2);
        assert_eq!(received.term, 1);
        assert_eq!(received.msg_type, MessageType::MsgHeartbeat as i32);

        // Clean up
        server_handle.abort();
    }
}
