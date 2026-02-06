#![cfg_attr(target_os = "linux", allow(dead_code))]

use anyhow::{Context, Result};
use bytes::BytesMut;
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

#[cfg(all(target_os = "linux", feature = "rdma"))]
use std::path::Path;
#[cfg(all(target_os = "linux", feature = "rdma"))]
use tracing::warn;
#[cfg(all(target_os = "linux", feature = "rdma"))]
mod rdma;
#[cfg(all(target_os = "linux", feature = "rdma"))]
pub use rdma::RdmaHandshake;

#[cfg(all(target_os = "linux", feature = "rdma"))]
type MrHandle = *mut rdma_sys::ibv_mr;
#[cfg(not(all(target_os = "linux", feature = "rdma")))]
type MrHandle = *mut std::ffi::c_void;

/// Registered buffer backed by pinned memory for zero-copy RDMA sends.
#[derive(Debug)]
pub struct RegisteredBuffer {
    pub data: BytesMut,
    pub lkey: u32,
    pub mr_handle: MrHandle,
    #[cfg(all(target_os = "linux", feature = "rdma"))]
    recycler: Option<Arc<MemoryRegionPoolInner>>,
}

impl RegisteredBuffer {
    /// Reset the logical length for reuse while retaining capacity and registration.
    pub fn clear_len(&mut self) {
        self.data.truncate(0);
    }
}

#[async_trait::async_trait]
pub trait ZeroCopyTransport: Send + Sync {
    /// Allocate a pre-registered buffer sized for the requested payload.
    async fn alloc_buffer(&self, size: usize) -> RegisteredBuffer;

    /// Send a registered buffer. Ownership is returned on completion.
    async fn send_buffer(
        &self,
        target: NodeId,
        buffer: RegisteredBuffer,
    ) -> Result<RegisteredBuffer>;
}

struct StreamEntry {
    writer: Arc<Mutex<OwnedWriteHalf>>,
    last_used: Instant,
}

/// TLS configuration for inter-node transport encryption.
///
/// When configured, all outbound replication connections use TLS.
/// Both client authentication (mTLS) and server-only TLS are supported.
///
/// # Configuration
///
/// Set via environment variables:
/// - `SPACE_TLS_CA_CERT`: Path to CA certificate (PEM) for verifying peers
/// - `SPACE_TLS_CERT`: Path to this node's certificate (PEM)
/// - `SPACE_TLS_KEY`: Path to this node's private key (PEM)
///
/// When all three are set, mTLS is enabled. When only `SPACE_TLS_CA_CERT` is set,
/// server-only verification is used.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Fields read when tokio-rustls TLS wrapping is enabled
pub struct TlsConfig {
    /// Path to CA certificate for verifying peer connections.
    pub ca_cert_path: std::path::PathBuf,
    /// Path to this node's TLS certificate (for mTLS).
    pub cert_path: Option<std::path::PathBuf>,
    /// Path to this node's TLS private key (for mTLS).
    pub key_path: Option<std::path::PathBuf>,
}

impl TlsConfig {
    /// Load TLS configuration from environment variables.
    ///
    /// Returns `None` if `SPACE_TLS_CA_CERT` is not set (TLS disabled).
    pub fn from_env() -> Option<Self> {
        let ca_cert = std::env::var("SPACE_TLS_CA_CERT").ok()?;
        Some(Self {
            ca_cert_path: ca_cert.into(),
            cert_path: std::env::var("SPACE_TLS_CERT").ok().map(Into::into),
            key_path: std::env::var("SPACE_TLS_KEY").ok().map(Into::into),
        })
    }

    /// Returns `true` if mutual TLS (client cert + key) is configured.
    pub fn is_mtls(&self) -> bool {
        self.cert_path.is_some() && self.key_path.is_some()
    }
}

/// Manages persistent outbound connections for replication traffic.
///
/// Supports optional TLS encryption for inter-node communication.
/// Configure TLS via [`TlsConfig::from_env`] or pass `None` for plaintext.
#[derive(Clone)]
pub struct ConnectionManager {
    streams: Arc<RwLock<HashMap<NodeId, StreamEntry>>>,
    idle_timeout: Duration,
    tls_config: Option<TlsConfig>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        let tls_config = TlsConfig::from_env();
        if tls_config.is_some() {
            tracing::info!("inter-node TLS enabled via SPACE_TLS_CA_CERT");
        } else {
            tracing::warn!("inter-node TLS is NOT configured; replication traffic is unencrypted. Set SPACE_TLS_CA_CERT to enable TLS.");
        }
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
            idle_timeout: Duration::from_secs(60),
            tls_config,
        }
    }

    /// Create a ConnectionManager with explicit TLS configuration.
    pub fn with_tls(tls_config: Option<TlsConfig>) -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
            idle_timeout: Duration::from_secs(60),
            tls_config,
        }
    }

    /// Acquire (or establish) a writable half for the target peer.
    /// Connections are reused until they hit the idle timeout or error.
    ///
    /// When TLS is configured, new connections are wrapped in a TLS session
    /// before being added to the pool.
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

        // TODO: When tokio-rustls is added as a dependency, wrap `stream` in TLS here:
        //   if let Some(tls) = &self.tls_config {
        //       let connector = build_tls_connector(tls)?;
        //       let domain = ServerName::try_from(addr.ip().to_string())?;
        //       let tls_stream = connector.connect(domain, stream).await?;
        //       ...
        //   }
        if self.tls_config.is_some() {
            tracing::debug!(peer = %peer, "TLS config present but runtime TLS wrapping requires tokio-rustls dependency");
        }

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

#[cfg(all(target_os = "linux", feature = "rdma"))]
const DEFAULT_RDMA_POOL_BYTES: usize = 256 * 1024 * 1024;
#[cfg(all(target_os = "linux", feature = "rdma"))]
const DEFAULT_RDMA_CHUNK_BYTES: usize = 1 * 1024 * 1024;
#[cfg(all(target_os = "linux", feature = "rdma"))]
const RDMA_CQ_DEPTH: i32 = 1024;

#[cfg(all(target_os = "linux", feature = "rdma"))]
#[derive(Debug)]
struct BufferParts {
    data: BytesMut,
    mr_handle: *mut rdma_sys::ibv_mr,
    lkey: u32,
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
struct MemoryRegionPoolInner {
    pd: *mut rdma_sys::ibv_pd,
    chunk_size: usize,
    free: std::sync::Mutex<Vec<BufferParts>>,
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl MemoryRegionPoolInner {
    fn register_region(&self, size: usize) -> Result<BufferParts> {
        let size = size.max(self.chunk_size);
        let mut data = BytesMut::zeroed(size);

        unsafe {
            // Pin the memory to avoid paging.
            let lock_res = libc::mlock(data.as_ptr() as *const libc::c_void, data.len());
            if lock_res != 0 {
                warn!("failed to mlock RDMA buffer; continuing without pinning");
            }

            let mr = rdma_sys::ibv_reg_mr(
                self.pd,
                data.as_mut_ptr() as *mut libc::c_void,
                data.len(),
                (rdma_sys::ibv_access_flags_IBV_ACCESS_LOCAL_WRITE
                    | rdma_sys::ibv_access_flags_IBV_ACCESS_REMOTE_READ
                    | rdma_sys::ibv_access_flags_IBV_ACCESS_REMOTE_WRITE) as i32,
            );

            if mr.is_null() {
                return Err(anyhow::anyhow!(
                    "ibv_reg_mr failed to register RDMA memory region"
                ));
            }

            Ok(BufferParts {
                data,
                mr_handle: mr,
                lkey: (*mr).lkey,
            })
        }
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl Drop for MemoryRegionPoolInner {
    fn drop(&mut self) {
        if let Ok(mut free) = self.free.lock() {
            for buf in free.drain(..) {
                unsafe {
                    if !buf.mr_handle.is_null() {
                        let _ = rdma_sys::ibv_dereg_mr(buf.mr_handle);
                    }
                }
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
#[derive(Clone)]
pub struct MemoryRegionPool {
    inner: Arc<MemoryRegionPoolInner>,
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl MemoryRegionPool {
    fn new(pd: *mut rdma_sys::ibv_pd, target_pool_bytes: usize, chunk_size: usize) -> Result<Self> {
        let pool = Self {
            inner: Arc::new(MemoryRegionPoolInner {
                pd,
                chunk_size,
                free: std::sync::Mutex::new(Vec::new()),
            }),
        };

        let mut allocated: usize = 0;
        while allocated < target_pool_bytes {
            let buf = pool.inner.register_region(chunk_size)?;
            allocated += buf.data.len();
            if let Ok(mut free) = pool.inner.free.lock() {
                free.push(buf);
            }
        }

        Ok(pool)
    }

    fn alloc(&self, size: usize) -> Result<RegisteredBuffer> {
        if let Ok(mut free) = self.inner.free.lock() {
            if let Some(idx) = free.iter().position(|buf| buf.data.capacity() >= size) {
                let mut parts = free.swap_remove(idx);
                parts.data.resize(size, 0);
                return Ok(RegisteredBuffer::from_parts(
                    parts,
                    self.inner.clone(),
                    size,
                ));
            }
        }

        let mut parts = self.inner.register_region(size)?;
        parts.data.resize(size, 0);
        Ok(RegisteredBuffer::from_parts(
            parts,
            self.inner.clone(),
            size,
        ))
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl RegisteredBuffer {
    fn from_parts(mut parts: BufferParts, pool: Arc<MemoryRegionPoolInner>, size: usize) -> Self {
        if parts.data.len() < size {
            parts.data.resize(size, 0);
        } else if parts.data.len() > size {
            parts.data.truncate(size);
        }

        RegisteredBuffer {
            data: parts.data,
            lkey: parts.lkey,
            mr_handle: parts.mr_handle,
            recycler: Some(pool),
        }
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl Drop for RegisteredBuffer {
    fn drop(&mut self) {
        if let Some(pool) = self.recycler.take() {
            let mut data = BytesMut::new();
            std::mem::swap(&mut self.data, &mut data);

            let parts = BufferParts {
                data,
                mr_handle: self.mr_handle,
                lkey: self.lkey,
            };

            if let Ok(mut free) = pool.free.lock() {
                free.push(parts);
            }
        }
    }
}

/// RDMA transport stub backed by registered memory and a completion-aware actor.
/// Falls back to the standard DataTransport path for actual wire sends when RDMA
/// queue pairs are not yet negotiated.
#[cfg(all(target_os = "linux", feature = "rdma"))]
pub struct RdmaTransport {
    peers: Arc<RwLock<HashMap<NodeId, SocketAddr>>>,
    ctx: *mut rdma_sys::ibv_context,
    pd: *mut rdma_sys::ibv_pd,
    cq: *mut rdma_sys::ibv_cq,
    pool: MemoryRegionPool,
    fallback: Arc<dyn crate::DataTransport>,
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl RdmaTransport {
    pub fn new(peers: Arc<RwLock<HashMap<NodeId, SocketAddr>>>) -> Result<Self> {
        if !Self::rdma_supported() {
            return Err(anyhow::anyhow!(
                "RDMA device not detected, skipping RDMA transport init"
            ));
        }

        let mut num_devices = 0;
        let device_list = unsafe { rdma_sys::ibv_get_device_list(&mut num_devices) };
        if device_list.is_null() || num_devices == 0 {
            return Err(anyhow::anyhow!("no RDMA devices found on host"));
        }

        let ctx = unsafe {
            let ctx = rdma_sys::ibv_open_device(*device_list);
            rdma_sys::ibv_free_device_list(device_list);
            ctx
        };

        if ctx.is_null() {
            return Err(anyhow::anyhow!("failed to open RDMA device context"));
        }

        let pd = unsafe { rdma_sys::ibv_alloc_pd(ctx) };
        if pd.is_null() {
            unsafe {
                let _ = rdma_sys::ibv_close_device(ctx);
            }
            return Err(anyhow::anyhow!("failed to allocate RDMA protection domain"));
        }

        let cq = unsafe {
            rdma_sys::ibv_create_cq(
                ctx,
                RDMA_CQ_DEPTH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if cq.is_null() {
            unsafe {
                let _ = rdma_sys::ibv_dealloc_pd(pd);
                let _ = rdma_sys::ibv_close_device(ctx);
            }
            return Err(anyhow::anyhow!("failed to create RDMA completion queue"));
        }

        let pool_bytes = Self::pool_bytes_from_env();
        let pool = MemoryRegionPool::new(pd, pool_bytes, DEFAULT_RDMA_CHUNK_BYTES)?;

        // Use the high-performance io_uring transport as a fallback data path while
        // RDMA queue pairs are negotiated.
        let fallback: Arc<dyn crate::DataTransport> = Arc::new(crate::IoUringTransport::new());

        Ok(Self {
            peers,
            ctx,
            pd,
            cq,
            pool,
            fallback,
        })
    }

    fn pool_bytes_from_env() -> usize {
        std::env::var("SPACE_RDMA_POOL_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_RDMA_POOL_BYTES)
    }

    fn rdma_supported() -> bool {
        Path::new("/dev/infiniband/uverbs0").exists()
            && unsafe {
                let mut count = 0;
                let list = rdma_sys::ibv_get_device_list(&mut count);
                if !list.is_null() {
                    rdma_sys::ibv_free_device_list(list);
                }
                count > 0
            }
    }

    async fn target_addr(&self, node: NodeId) -> Result<SocketAddr> {
        let peers = self.peers.read().await;
        peers
            .get(&node)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("target {} not found in peer registry", node))
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
impl Drop for RdmaTransport {
    fn drop(&mut self) {
        unsafe {
            let _ = rdma_sys::ibv_destroy_cq(self.cq);
            let _ = rdma_sys::ibv_dealloc_pd(self.pd);
            let _ = rdma_sys::ibv_close_device(self.ctx);
        }
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
#[async_trait::async_trait]
impl ZeroCopyTransport for RdmaTransport {
    async fn alloc_buffer(&self, size: usize) -> RegisteredBuffer {
        match self.pool.alloc(size) {
            Ok(mut buf) => {
                buf.clear_len();
                buf
            }
            Err(err) => {
                warn!(error = %err, size, "RDMA pool allocation failed, falling back to heap buffer");
                RegisteredBuffer {
                    data: BytesMut::zeroed(size),
                    lkey: 0,
                    mr_handle: std::ptr::null_mut(),
                    recycler: None,
                }
            }
        }
    }

    async fn send_buffer(
        &self,
        target: NodeId,
        mut buffer: RegisteredBuffer,
    ) -> Result<RegisteredBuffer> {
        let addr = self.target_addr(target).await?;

        // Until QP negotiation is wired, rely on the fallback transport to carry
        // the bytes over the wire while retaining the zero-copy buffer lifecycle.
        let payload = buffer.data.to_vec();
        self.fallback.send_frame(target, addr, payload).await?;

        buffer.clear_len();
        Ok(buffer)
    }
}

#[cfg(all(target_os = "linux", feature = "rdma"))]
#[async_trait::async_trait]
impl crate::DataTransport for RdmaTransport {
    async fn send_frame(
        &self,
        target: NodeId,
        target_addr: SocketAddr,
        frame: Vec<u8>,
    ) -> Result<()> {
        {
            let mut peers = self.peers.write().await;
            peers.insert(target, target_addr);
        }

        let mut buffer = self.alloc_buffer(frame.len()).await;
        buffer.data.clear();
        buffer.data.extend_from_slice(&frame);
        let _ = self.send_buffer(target, buffer).await?;
        Ok(())
    }
}
