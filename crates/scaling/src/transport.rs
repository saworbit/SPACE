#![cfg_attr(target_os = "linux", allow(dead_code))]

use anyhow::{Context, Result};
use common::podms::NodeId;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(test)]
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock};

struct StreamEntry {
    writer: Arc<Mutex<OwnedWriteHalf>>,
    last_used: Instant,
}

/// Manages persistent outbound connections for replication traffic.
#[derive(Clone)]
pub struct ConnectionManager {
    streams: Arc<RwLock<HashMap<NodeId, StreamEntry>>>,
    idle_timeout: Duration,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
            idle_timeout: Duration::from_secs(60),
        }
    }

    /// Acquire (or establish) a writable half for the target peer.
    /// Connections are reused until they hit the idle timeout or error.
    pub async fn get_writer(
        &self,
        peer: NodeId,
        addr: SocketAddr,
    ) -> Result<Arc<Mutex<OwnedWriteHalf>>> {
        let mut streams = self.streams.write().await;

        if let Some(entry) = streams.get_mut(&peer) {
            if entry.last_used.elapsed() <= self.idle_timeout {
                entry.last_used = Instant::now();
                return Ok(entry.writer.clone());
            }

            // Drop idle connection before establishing a new one.
            streams.remove(&peer);
        }

        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("failed to connect to peer {} at {}", peer, addr))?;
        stream
            .set_nodelay(true)
            .context("failed to disable Nagle's algorithm")?;

        let (_, write_half) = stream.into_split();
        let writer = Arc::new(Mutex::new(write_half));

        streams.insert(
            peer,
            StreamEntry {
                writer: writer.clone(),
                last_used: Instant::now(),
            },
        );

        Ok(writer)
    }

    #[cfg(test)]
    pub async fn shutdown_writer(&self, peer: NodeId) {
        if let Some(writer) = {
            let streams = self.streams.read().await;
            streams.get(&peer).map(|entry| entry.writer.clone())
        } {
            let mut guard = writer.lock().await;
            let _ = guard.shutdown().await;
        }
    }

    /// Remove a connection from the pool so the next send reconnects.
    pub async fn invalidate(&self, peer: NodeId) {
        self.streams.write().await.remove(&peer);
    }
}
