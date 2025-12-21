use anyhow::{anyhow, Context, Result};
#[cfg(feature = "modular_pipeline")]
use capsule_registry::modular_pipeline;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
#[cfg(feature = "phase4")]
use clap::Args;
use clap::{Parser, Subcommand};
#[cfg(feature = "phase4")]
use common::podms::{Telemetry, ZoneId};
use common::CapsuleId;
use common::{Policy, SegmentId};
#[cfg(feature = "phase4")]
use csi_driver_rs::ProvisionRequest;
#[cfg(feature = "phase4")]
use encryption::keymanager::KeyManager;
#[cfg(feature = "phase4")]
use federation::FederationBridge;
use nvram_sim::NvramLog;
use protocol_block::BlockView;
#[cfg(feature = "phase4")]
use protocol_csi::csi_provision_capsule;
#[cfg(feature = "phase4")]
use protocol_fuse::{mount_capsule_fuse, mount_fuse_view};
#[cfg(feature = "phase4")]
use protocol_nfs::phase4::export_nfs_view;
use protocol_nfs::NfsView;
#[cfg(feature = "phase4")]
use protocol_nvme::NvmeView;
#[cfg(feature = "phase4")]
use scaling::ContentStore;
#[cfg(feature = "phase4")]
use scaling::MeshNode;
use std::fs;
use std::io::{self, Write};
use std::net::SocketAddr;
#[cfg(feature = "phase4")]
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
#[cfg(feature = "phase4")]
use std::sync::Arc;
use std::sync::Once;
#[cfg(feature = "phase4")]
use std::time::Duration;
#[cfg(feature = "phase4")]
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use gossip_layer::GossipImpl;
use mesh_core::{GossipConfig, NodeRole, Peer, PeerStore};

const NVRAM_PATH: &str = "space.nvram";
const REGISTRY_DB_PATH: &str = "space.db";
const NFS_NAMESPACE_FILE: &str = "space.nfs.json";
const BLOCK_METADATA_FILE: &str = "space.block.json";
const LIST_PAGE_SIZE: usize = 256;

fn sanitize_zone_component(zone: &str) -> String {
    let mut out = String::with_capacity(zone.len());
    for ch in zone.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".into()
    } else {
        out
    }
}

fn registry_path_for_zone(zone: Option<&str>) -> PathBuf {
    match zone {
        Some(zone) => PathBuf::from(format!("space.{}.db", sanitize_zone_component(zone))),
        None => PathBuf::from(REGISTRY_DB_PATH),
    }
}

fn nvram_path_for_zone(zone: Option<&str>) -> PathBuf {
    match zone {
        Some(zone) => PathBuf::from(format!("space.{}.nvram", sanitize_zone_component(zone))),
        None => PathBuf::from(NVRAM_PATH),
    }
}

fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let format = std::env::var("SPACE_LOG_FORMAT").unwrap_or_else(|_| "compact".to_string());

        if format.eq_ignore_ascii_case("json") {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter.clone())
                .with_target(true)
                .json()
                .flatten_event(true)
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_target(true)
                .compact()
                .init();
        }
    });
}

#[derive(Parser)]
#[command(name = "spacectl")]
#[command(about = "SPACE storage control utility", long_about = None)]
struct Cli {
    /// Bearer token for authenticated control-plane calls (optional)
    #[arg(long)]
    token: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[cfg(feature = "phase4")]
#[derive(Clone, Debug, Args)]
struct ProjectArgs {
    /// Optional subcommand (e.g. `mount`).
    #[command(subcommand)]
    command: Option<ProjectCommand>,
    /// Protocol view to project (nvme, nfs, fuse, csi).
    #[arg(long)]
    view: Option<String>,
    /// Capsule UUID to materialize.
    #[arg(long)]
    id: Option<Uuid>,
    /// YAML policy file driving the projection.
    #[arg(long)]
    policy_file: Option<PathBuf>,
    /// Optional zone name (backed by `space.<zone>.db` / `space.<zone>.nvram`).
    #[arg(long)]
    zone: Option<String>,
}

#[cfg(feature = "phase4")]
#[derive(Clone, Debug, Subcommand)]
enum ProjectCommand {
    /// Mount a capsule into a local directory as a file-style view.
    Mount {
        /// Capsule UUID to materialize.
        #[arg(long)]
        id: Uuid,
        /// Target directory to project into (creates `content` inside).
        #[arg(long)]
        target: PathBuf,
        /// Optional zone name (backed by `space.<zone>.db` / `space.<zone>.nvram`).
        #[arg(long)]
        zone: Option<String>,
        /// Optional YAML policy override used for view enforcement/federation.
        #[arg(long)]
        policy_file: Option<PathBuf>,
    },
}

#[cfg(feature = "phase4")]
#[derive(Clone, Default)]
struct DummyContentStore;

#[cfg(feature = "phase4")]
impl ContentStore for DummyContentStore {
    fn lookup_content(&self, _hash: &common::ContentHash) -> Option<common::SegmentId> {
        None
    }

    fn register_content(&self, _hash: &common::ContentHash, _segment_id: common::SegmentId) {}
}

#[derive(Subcommand)]
enum NfsCommands {
    /// Materialise a directory hierarchy
    Mkdir {
        /// Directory path (POSIX-style)
        #[arg(short, long)]
        path: String,
    },
    /// Write a file from the local filesystem into the namespace
    Write {
        #[arg(short, long)]
        path: String,
        /// Source file path
        #[arg(short, long)]
        file: String,
    },
    /// Read a file and stream it to stdout
    Read {
        #[arg(short, long)]
        path: String,
    },
    /// List the entries beneath a directory
    List {
        #[arg(short, long, default_value = "/")]
        path: String,
    },
    /// Remove a file or empty directory
    Delete {
        #[arg(short, long)]
        path: String,
    },
    /// Show metadata for a path
    Metadata {
        #[arg(short, long)]
        path: String,
    },
}

#[derive(Subcommand)]
enum BlockCommands {
    /// Create a new logical volume
    Create {
        name: String,
        size: u64,
        #[arg(long)]
        block_size: Option<u64>,
    },
    /// Delete a volume
    Delete { name: String },
    /// List all volumes
    List,
    /// Describe a single volume
    Info { name: String },
    /// Read bytes from a volume (writes to stdout)
    Read {
        name: String,
        offset: u64,
        #[arg(long)]
        length: usize,
    },
    /// Write bytes from a file into a volume
    Write {
        name: String,
        offset: u64,
        #[arg(short, long)]
        file: String,
    },
}

#[derive(Subcommand)]
enum ServerCommands {
    /// Start a SPACE node (gossip + raft).
    Start {
        /// Numeric node id used by the Raft cluster.
        #[arg(long)]
        node_id: u64,

        /// Gossip listen address (libp2p TCP).
        #[arg(long, default_value = "0.0.0.0:7000")]
        gossip_addr: SocketAddr,

        /// Raft gRPC listen address.
        #[arg(long, default_value = "0.0.0.0:9000")]
        raft_addr: SocketAddr,

        /// Path to the capsule metadata store (sled).
        #[arg(long, default_value = "space.db")]
        metadata_path: String,

        /// Path to the raft log/state store (sled).
        #[arg(long, default_value = "space.raft.db")]
        raft_store_path: String,

        /// Bootstrap a new cluster (becomes leader term 1).
        #[arg(long)]
        bootstrap: bool,

        /// Join an existing cluster by contacting a known raft node.
        #[arg(long)]
        join: Option<SocketAddr>,

        /// Gossip seed peer(s) to dial (repeatable).
        #[arg(long)]
        gossip_seed: Vec<SocketAddr>,
    },

    /// Query cluster status from a raft node.
    Status {
        /// Raft gRPC address of any cluster node.
        #[arg(long)]
        addr: SocketAddr,
    },
}

#[derive(Subcommand)]
enum RegistryCommands {
    /// Create/update a capsule's metadata through the Raft leader.
    Put {
        /// Raft gRPC address of any cluster node.
        #[arg(long)]
        addr: SocketAddr,

        /// Capsule UUID.
        #[arg(long)]
        id: String,

        /// Capsule size in bytes.
        #[arg(long, default_value_t = 0)]
        size: u64,

        /// Segment id(s) belonging to the capsule (repeatable).
        #[arg(long)]
        segment: Vec<u64>,

        /// Optional YAML policy file to attach.
        #[arg(long)]
        policy_file: Option<String>,
    },

    /// Fetch capsule metadata from a node's local state machine.
    Get {
        /// Raft gRPC address of any cluster node.
        #[arg(long)]
        addr: SocketAddr,

        /// Capsule UUID.
        #[arg(long)]
        id: String,
    },

    /// Delete capsule metadata through the Raft leader.
    Delete {
        /// Raft gRPC address of any cluster node.
        #[arg(long)]
        addr: SocketAddr,

        /// Capsule UUID.
        #[arg(long)]
        id: String,
    },
}

#[cfg(feature = "phase4")]
#[derive(Subcommand)]
enum SnapshotCommands {
    /// Force immediate execution of a capsule's replication policy.
    Trigger {
        /// Capsule UUID to snapshot/replicate now.
        #[arg(long, value_name = "UUID")]
        id: String,
        /// Optional override for the RPO interval (seconds).
        #[arg(long, value_name = "SECONDS")]
        rpo_secs: Option<u64>,
        /// Wait briefly after emitting the telemetry event.
        #[arg(long)]
        wait: bool,
        /// Where to append the serialized telemetry event.
        #[arg(long, default_value = "space.telemetry.jsonl")]
        out: String,
    },
}

fn open_registry_and_nvram_for_zone(zone: Option<&str>) -> Result<(CapsuleRegistry, NvramLog)> {
    let registry_path = registry_path_for_zone(zone);
    let nvram_path = nvram_path_for_zone(zone);
    let registry = CapsuleRegistry::open(&registry_path)?;
    let nvram = NvramLog::open(&nvram_path)?;
    Ok((registry, nvram))
}

fn open_registry_and_nvram() -> Result<(CapsuleRegistry, NvramLog)> {
    open_registry_and_nvram_for_zone(None)
}

#[cfg(feature = "modular_pipeline")]
fn build_modular_pipeline_handle(
    registry: CapsuleRegistry,
) -> Result<modular_pipeline::RegistryPipelineHandle> {
    modular_pipeline::registry_pipeline_from_env(NVRAM_PATH, registry)
}

#[cfg(feature = "modular_pipeline")]
async fn modular_write_capsule(data: &[u8]) -> Result<CapsuleId> {
    let registry = CapsuleRegistry::new();
    let mut handle = build_modular_pipeline_handle(registry)?;
    handle.write_capsule(data, &Policy::default()).await
}

#[cfg(feature = "modular_pipeline")]
async fn modular_read_capsule(id: CapsuleId) -> Result<Vec<u8>> {
    let registry = CapsuleRegistry::new();
    let handle = build_modular_pipeline_handle(registry)?;
    handle.read_capsule(id).await
}

async fn run_nfs_command(command: NfsCommands) -> Result<()> {
    let (registry, nvram) = open_registry_and_nvram()?;
    let nfs = NfsView::open(registry, nvram, NFS_NAMESPACE_FILE)?;

    match command {
        NfsCommands::Mkdir { path } => {
            nfs.mkdir(&path).await?;
            println!("Created directory tree: {}", path);
        }
        NfsCommands::Write { path, file } => {
            let data = fs::read(&file)?;
            let capsule = nfs.write_file(&path, data).await?;
            println!(
                "Wrote {} (capsule {}) from {}",
                path,
                capsule.as_uuid(),
                file
            );
        }
        NfsCommands::Read { path } => {
            let data = nfs.read_file(&path).await?;
            io::stdout().write_all(&data)?;
        }
        NfsCommands::List { path } => {
            let entries = nfs.list_directory(&path).await?;
            if entries.is_empty() {
                println!("(empty directory)");
            } else {
                println!("Type\tSize (bytes)\tPath\tCapsule");
                for entry in entries {
                    let kind = if entry.is_directory() { "dir " } else { "file" };
                    let capsule = entry
                        .capsule_id()
                        .map(|id| id.as_uuid().to_string())
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{}\t{:>12}\t{}\t{}",
                        kind,
                        entry.size(),
                        entry.path(),
                        capsule
                    );
                }
            }
        }
        NfsCommands::Delete { path } => {
            nfs.delete(&path).await?;
            println!("Deleted {}", path);
        }
        NfsCommands::Metadata { path } => {
            let entry = nfs.metadata(&path).await?;
            let kind = if entry.is_directory() {
                "directory"
            } else {
                "file"
            };
            println!("Path: {}", entry.path());
            println!("Type: {}", kind);
            println!("Size: {}", entry.size());
            println!("Created: {}", entry.created_at());
            println!("Modified: {}", entry.modified_at());
            if let Some(id) = entry.capsule_id() {
                println!("Capsule: {}", id.as_uuid());
            }
        }
    }

    Ok(())
}

async fn run_block_command(command: BlockCommands) -> Result<()> {
    let (registry, nvram) = open_registry_and_nvram()?;
    let block = BlockView::open(registry, nvram, BLOCK_METADATA_FILE)?;

    match command {
        BlockCommands::Create {
            name,
            size,
            block_size,
        } => {
            let volume = if let Some(block_size) = block_size {
                block
                    .create_volume_with_block_size(&name, size, block_size)
                    .await?
            } else {
                block.create_volume(&name, size).await?
            };
            println!(
                "Created volume {} (size={} bytes, block_size={})",
                volume.name(),
                volume.size(),
                volume.block_size()
            );
        }
        BlockCommands::Delete { name } => {
            block.delete_volume(&name).await?;
            println!("Deleted volume {}", name);
        }
        BlockCommands::List => {
            let volumes = block.list_volumes().await;
            if volumes.is_empty() {
                println!("(no volumes)");
            } else {
                println!("Name\tSize (bytes)\tBlock Size\tCapsule");
                for volume in volumes {
                    println!(
                        "{}\t{:>12}\t{:>10}\t{}",
                        volume.name(),
                        volume.size(),
                        volume.block_size(),
                        volume.capsule_id().as_uuid()
                    );
                }
            }
        }
        BlockCommands::Info { name } => {
            let volume = block.volume(&name).await?;
            println!("Name: {}", volume.name());
            println!("Size: {}", volume.size());
            println!("Block Size: {}", volume.block_size());
            println!("Capsule: {}", volume.capsule_id().as_uuid());
            println!("Created: {}", volume.created_at());
            println!("Updated: {}", volume.updated_at());
            println!("Version: {}", volume.version());
        }
        BlockCommands::Read {
            name,
            offset,
            length,
        } => {
            let data = block.read(&name, offset, length).await?;
            io::stdout().write_all(&data)?;
        }
        BlockCommands::Write { name, offset, file } => {
            let data = fs::read(&file)?;
            block.write(&name, offset, &data).await?;
            println!(
                "Wrote {} bytes to volume {} from {}",
                data.len(),
                name,
                file
            );
        }
    }

    Ok(())
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new capsule from data
    Create {
        /// Input file path
        #[arg(short, long)]
        file: String,
        #[cfg(feature = "modular_pipeline")]
        #[arg(long)]
        modular: bool,
    },
    /// Store a local file as a capsule (Phase 4 projection tests use this shape).
    Put {
        /// Input file path
        file: String,
        /// Optional capsule UUID to use (default: generate).
        #[arg(long)]
        id: Option<String>,
        /// Optional YAML policy file to attach.
        #[arg(long)]
        policy_file: Option<PathBuf>,
        /// Optional zone name (backed by `space.<zone>.db` / `space.<zone>.nvram`).
        #[arg(long)]
        zone: Option<String>,
    },
    /// Read capsule contents
    Read {
        /// Capsule UUID
        capsule_id: String,
        #[cfg(feature = "modular_pipeline")]
        #[arg(long)]
        modular: bool,
    },
    /// List all capsules
    List,
    /// Start S3-compatible HTTP server
    ServeS3 {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
        #[cfg(feature = "modular_pipeline")]
        #[arg(long)]
        modular: bool,
    },
    #[cfg(feature = "phase4")]
    Project(ProjectArgs),
    #[cfg(feature = "phase4")]
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommands,
    },
    /// Interact with the NFS namespace view
    Nfs {
        #[command(subcommand)]
        command: NfsCommands,
    },
    /// Manage block-backed volumes
    Block {
        #[command(subcommand)]
        command: BlockCommands,
    },
    /// Cluster server operations.
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    /// Operate on capsule metadata via Raft.
    Registry {
        #[command(subcommand)]
        command: RegistryCommands,
    },
}

fn load_policy_file(path: &PathBuf) -> Result<Policy> {
    let text = fs::read_to_string(path)?;
    serde_yaml::from_str(&text).map_err(|err| anyhow!(err))
}

#[cfg(feature = "phase4")]
async fn handle_project_command(args: ProjectArgs) -> Result<()> {
    if let Some(ProjectCommand::Mount {
        id,
        target,
        zone,
        policy_file,
    }) = args.command
    {
        let capsule_id = CapsuleId::from_uuid(id);
        let (registry, nvram) = open_registry_and_nvram_for_zone(zone.as_deref())?;
        let registry = Arc::new(registry);
        let capsule = registry
            .lookup(capsule_id)
            .with_context(|| format!("capsule {} not found", capsule_id.as_uuid()))?;

        fs::create_dir_all(&target)
            .with_context(|| format!("create mount target {}", target.display()))?;

        let pipeline = Arc::new(WritePipeline::new(registry.as_ref().clone(), nvram.clone()));
        println!(
            "Mounting capsule {} at {} (read-only FUSE: {}/content)",
            capsule_id.as_uuid(),
            target.display(),
            target.display()
        );

        match mount_capsule_fuse(Arc::clone(&pipeline), capsule_id, capsule.size, &target) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("Kernel FUSE mount unavailable ({err}); falling back to file view.");
            }
        }

        let policy = match policy_file {
            Some(path) => load_policy_file(&path)?,
            None => capsule.policy.clone(),
        };

        let content_store = Arc::new(RwLock::new(DummyContentStore));
        let nvram_log = Arc::new(RwLock::new(nvram.clone()));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));
        let zone_name = zone.unwrap_or_else(|| "local".into());

        let mesh = MeshNode::new(
            ZoneId::Metro { name: zone_name },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            content_store,
            nvram_log,
            key_manager,
        )
        .await?;

        let pipeline = Arc::clone(&pipeline);
        let view = mount_fuse_view(
            capsule_id,
            &policy,
            &mesh,
            pipeline,
            &target,
            registry.as_ref(),
        )
        .await?;

        println!(
            "Mounted capsule {} at {}",
            capsule_id.as_uuid(),
            view.mountpoint().display()
        );
        tokio::signal::ctrl_c().await?;
        let _ = view.unmount().await;
        return Ok(());
    }

    let capsule_id = CapsuleId::from_uuid(
        args.id
            .ok_or_else(|| anyhow!("--id is required unless using `spacectl project mount`"))?,
    );
    let view = args
        .view
        .clone()
        .ok_or_else(|| anyhow!("--view is required unless using `spacectl project mount`"))?
        .to_lowercase();
    let policy = match args.policy_file.as_ref() {
        Some(path) => load_policy_file(path)?,
        None => Policy::default(),
    };

    let (registry, nvram) = open_registry_and_nvram_for_zone(args.zone.as_deref())?;
    let registry = Arc::new(registry);
    let pipeline = Arc::new(WritePipeline::new(registry.as_ref().clone(), nvram.clone()));

    let content_store = Arc::new(RwLock::new(DummyContentStore));
    let nvram_log = Arc::new(RwLock::new(nvram));
    let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));
    let zone_name = args.zone.clone().unwrap_or_else(|| "local".into());

    let mesh = MeshNode::new(
        ZoneId::Metro { name: zone_name },
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        content_store,
        nvram_log,
        key_manager,
    )
    .await?;

    match view.as_str() {
        "nvme" => {
            let capsule = registry
                .lookup(capsule_id)
                .with_context(|| format!("capsule {} not found", capsule_id.as_uuid()))?;
            let nvme_view = NvmeView::project(&capsule, &policy, &mesh, registry.as_ref()).await?;
            println!("NVMe Target Active: {}", nvme_view.nqn());
            tokio::signal::ctrl_c().await?;
            drop(nvme_view);
        }
        "nfs" => {
            registry
                .lookup(capsule_id)
                .with_context(|| format!("capsule {} not found", capsule_id.as_uuid()))?;
            let server = export_nfs_view(capsule_id, &policy, &mesh, registry.as_ref()).await?;
            println!(
                "NFS Export Active: nfs://127.0.0.1:2049/{}",
                capsule_id.as_uuid()
            );
            tokio::signal::ctrl_c().await?;
            drop(server);
        }
        "fuse" => {
            let capsule = registry
                .lookup(capsule_id)
                .with_context(|| format!("capsule {} not found", capsule_id.as_uuid()))?;
            let mountpoint = std::env::temp_dir().join(format!("space-{}", capsule_id.as_uuid()));
            fs::create_dir_all(&mountpoint)
                .with_context(|| format!("create mount target {}", mountpoint.display()))?;
            println!(
                "Mounting capsule {} at {} (read-only FUSE: {}/content)",
                capsule_id.as_uuid(),
                mountpoint.display(),
                mountpoint.display()
            );

            match mount_capsule_fuse(Arc::clone(&pipeline), capsule_id, capsule.size, &mountpoint) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    eprintln!("Kernel FUSE mount unavailable ({err}); falling back to file view.");
                }
            }

            let view = mount_fuse_view(
                capsule_id,
                &policy,
                &mesh,
                pipeline,
                &mountpoint,
                registry.as_ref(),
            )
            .await?;
            println!("View ready at {}", view.mountpoint().display());
            tokio::signal::ctrl_c().await?;
            let _ = view.unmount().await;
        }
        "csi" => {
            registry
                .lookup(capsule_id)
                .with_context(|| format!("capsule {} not found", capsule_id.as_uuid()))?;
            let req = ProvisionRequest::from_capsule(&capsule_id.as_uuid().to_string());
            let server = csi_provision_capsule(req, &policy, &mesh, registry.as_ref()).await?;
            println!("CSI driver active for capsule {}", server.capsule_id());
            tokio::signal::ctrl_c().await?;
            drop(server);
        }
        other => return Err(anyhow!("Unknown view type: {}", other)),
    }

    Ok(())
}

#[cfg(feature = "phase4")]
async fn handle_snapshot_command(command: SnapshotCommands) -> Result<()> {
    match command {
        SnapshotCommands::Trigger {
            id,
            rpo_secs,
            wait,
            out,
        } => {
            let uuid = Uuid::parse_str(&id).map_err(|err| anyhow!(err))?;
            let capsule_id = CapsuleId::from_uuid(uuid);
            let forced_rpo = rpo_secs.map(Duration::from_secs);
            let telemetry = Telemetry::ForcePolicyExecution {
                capsule_id,
                forced_rpo,
            };

            let serialized = serde_json::to_string(&telemetry)?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&out)?;
            writeln!(file, "{}", serialized)?;

            println!(
                "Queued forced snapshot for capsule {} (forced_rpo={}) -> {}",
                capsule_id.as_uuid(),
                forced_rpo
                    .map(|d| format!("{:?}", d))
                    .unwrap_or_else(|| "policy".to_string()),
                out
            );

            if wait {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let token = cli
        .token
        .clone()
        .or_else(|| std::env::var("SPACE_AUTH_TOKEN").ok());
    if let Some(token) = token {
        // Surface the token to any future HTTP clients without changing existing flows.
        std::env::set_var("SPACE_AUTH_TOKEN", &token);
    }

    match cli.command {
        Commands::Create {
            file,
            #[cfg(feature = "modular_pipeline")]
            modular,
        } => {
            let data = fs::read(&file)?;
            #[cfg(feature = "modular_pipeline")]
            if modular {
                let id = modular_write_capsule(&data).await?;
                println!("Capsule created: {}", id.as_uuid());
                println!("Size: {} bytes", data.len());
                return Ok(());
            }

            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::new(registry, nvram);
            let id = pipeline.write_capsule(&data).await?;
            println!("Capsule created: {}", id.as_uuid());
            println!("Size: {} bytes", data.len());
        }
        Commands::Put {
            file,
            id,
            policy_file,
            zone,
        } => {
            let data = fs::read(&file)?;
            let uuid = match id {
                Some(id) => Uuid::parse_str(&id).with_context(|| format!("invalid UUID: {id}"))?,
                None => Uuid::new_v4(),
            };
            let capsule_id = CapsuleId::from_uuid(uuid);

            let policy = match policy_file {
                Some(path) => load_policy_file(&path)?,
                None => Policy::default(),
            };

            let (registry, nvram) = open_registry_and_nvram_for_zone(zone.as_deref())?;
            let seg_id: SegmentId = registry.alloc_segment()?;
            nvram.append(seg_id, &data)?;
            registry.create_capsule_with_segments(
                capsule_id,
                data.len() as u64,
                vec![seg_id],
                policy.clone(),
            )?;

            println!("{}", capsule_id.as_uuid());

            #[cfg(feature = "phase4")]
            if policy.federation.is_some() {
                let bridge = FederationBridge::new(std::env::current_dir()?);
                let result = bridge
                    .apply_policy(capsule_id, &policy, &registry, &nvram)
                    .await?;
                if result.zones_attempted > 0 {
                    println!(
                        "Federation: succeeded {}/{} zones",
                        result.zones_succeeded, result.zones_attempted
                    );
                }
            }
        }
        Commands::Read {
            capsule_id,
            #[cfg(feature = "modular_pipeline")]
            modular,
        } => {
            let uuid = capsule_id.parse()?;
            let id = CapsuleId::from_uuid(uuid);

            #[cfg(feature = "modular_pipeline")]
            if modular {
                let data = modular_read_capsule(id).await?;
                io::stdout().write_all(&data)?;
                return Ok(());
            }

            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::new(registry, nvram);
            let data = pipeline.read_capsule(id).await?;
            io::stdout().write_all(&data)?;
        }
        Commands::List => {
            let registry = CapsuleRegistry::new();
            let mut cursor = None;
            let mut printed = false;

            loop {
                let capsule_ids = registry.list_capsules(LIST_PAGE_SIZE, cursor)?;
                if capsule_ids.is_empty() {
                    break;
                }

                if !printed {
                    println!("Capsule ID\tSize (bytes)\tSegments");
                    printed = true;
                }

                cursor = capsule_ids.last().copied();

                for id in capsule_ids {
                    match registry.lookup(id) {
                        Ok(capsule) => {
                            println!(
                                "{}\t{:>12}\t{:>3}",
                                capsule.id.as_uuid(),
                                capsule.size,
                                capsule.segments.len()
                            );
                        }
                        Err(err) => {
                            println!("{} \t<error: {}>", id.as_uuid(), err);
                        }
                    }
                }
            }

            if !printed {
                println!("(no capsules)");
            }
        }
        Commands::ServeS3 {
            port,
            #[cfg(feature = "modular_pipeline")]
            modular,
        } => {
            use protocol_s3::{server::S3Server, S3View};

            println!("Starting SPACE S3 Protocol View...");

            #[cfg(feature = "modular_pipeline")]
            let s3_view = if modular {
                let registry = CapsuleRegistry::new();
                let handle = modular_pipeline::registry_pipeline_from_env(NVRAM_PATH, registry)?;
                S3View::new_modular(handle)
            } else {
                let registry = CapsuleRegistry::new();
                let nvram = NvramLog::open(NVRAM_PATH)?;
                S3View::new(registry, nvram)
            };

            #[cfg(not(feature = "modular_pipeline"))]
            let s3_view = {
                let registry = CapsuleRegistry::new();
                let nvram = NvramLog::open(NVRAM_PATH)?;
                S3View::new(registry, nvram)
            };

            let server = S3Server::new(s3_view, port);

            server.run().await?;
        }
        #[cfg(feature = "phase4")]
        Commands::Project(args) => {
            handle_project_command(args).await?;
        }
        #[cfg(feature = "phase4")]
        Commands::Snapshot { command } => {
            handle_snapshot_command(command).await?;
        }
        Commands::Nfs { command } => {
            run_nfs_command(command).await?;
        }
        Commands::Block { command } => {
            run_block_command(command).await?;
        }
        Commands::Server { command } => match command {
            ServerCommands::Start {
                node_id,
                gossip_addr,
                raft_addr,
                metadata_path,
                raft_store_path,
                bootstrap,
                join,
                gossip_seed,
            } => {
                if bootstrap && join.is_some() {
                    anyhow::bail!("--bootstrap and --join are mutually exclusive");
                }
                if !bootstrap && join.is_none() {
                    anyhow::bail!("either --bootstrap or --join <addr> is required");
                }

                let peer_store = PeerStore::new();
                let local_peer = Peer::new(node_id.to_string(), gossip_addr, NodeRole::StorageNode);

                let gossip = GossipImpl::with_peer_store(
                    GossipConfig::default(),
                    local_peer,
                    raft_addr.port(),
                    peer_store.clone(),
                )
                .await?;

                // Dial seeds from CLI and env `GOSSIP_SEEDS` (comma-separated).
                let mut seeds = gossip_seed;
                if let Ok(env_seeds) = std::env::var("GOSSIP_SEEDS") {
                    for item in env_seeds
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        if let Ok(addr) = item.parse::<SocketAddr>() {
                            seeds.push(addr);
                        }
                    }
                }
                for seed in seeds {
                    let _ = gossip.dial(seed).await;
                }

                let raft = if bootstrap {
                    capsule_registry::mesh::MeshRegistryRaft::start(
                        node_id,
                        raft_addr,
                        &metadata_path,
                        &raft_store_path,
                        true,
                    )
                    .await?
                } else {
                    let raft = capsule_registry::mesh::MeshRegistryRaft::start(
                        node_id,
                        raft_addr,
                        &metadata_path,
                        &raft_store_path,
                        false,
                    )
                    .await?;

                    capsule_registry::mesh::join_cluster(join.unwrap(), node_id, raft_addr).await?;
                    raft
                };

                // Forward gossip events into the registry peer monitor.
                let mut events = gossip.subscribe_events();
                let (tx, rx) = tokio::sync::mpsc::channel(256);
                tokio::spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(ev) => {
                                if tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });

                tokio::spawn(capsule_registry::mesh::monitor_peers(raft.raft.clone(), rx));

                tracing::info!(
                    node_id = node_id,
                    gossip_addr = %gossip_addr,
                    raft_addr = %raft_addr,
                    bootstrap = bootstrap,
                    "SPACE node started"
                );

                tokio::signal::ctrl_c().await?;
            }
            ServerCommands::Status { addr } => {
                let status = capsule_registry::mesh::cluster_status(addr).await?;
                println!("leader_id: {}", status.leader_id);
                println!("voters: {:?}", status.voters);
                println!("learners: {:?}", status.learners);
            }
        },
        Commands::Registry { command } => match command {
            RegistryCommands::Put {
                addr,
                id,
                size,
                segment,
                policy_file,
            } => {
                let uuid = id.parse()?;
                let capsule_id = CapsuleId::from_uuid(uuid);
                let policy = if let Some(path) = policy_file {
                    let text = fs::read_to_string(path)?;
                    serde_yaml::from_str::<Policy>(&text)?
                } else {
                    Policy::default()
                };

                let capsule = common::Capsule {
                    id: capsule_id,
                    size,
                    segments: segment.into_iter().map(common::SegmentId).collect(),
                    created_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs(),
                    policy,
                    deduped_bytes: 0,
                };

                capsule_registry::mesh::put_capsule(addr, capsule).await?;
                println!("ok");
            }
            RegistryCommands::Get { addr, id } => {
                let uuid = id.parse()?;
                let capsule_id = CapsuleId::from_uuid(uuid);
                match capsule_registry::mesh::get_capsule(addr, capsule_id).await? {
                    Some(capsule) => {
                        println!("{}", serde_json::to_string_pretty(&capsule)?);
                    }
                    None => {
                        println!("(not found)");
                    }
                }
            }
            RegistryCommands::Delete { addr, id } => {
                let uuid = id.parse()?;
                let capsule_id = CapsuleId::from_uuid(uuid);
                match capsule_registry::mesh::delete_capsule(addr, capsule_id).await? {
                    Some(capsule) => {
                        println!("{}", serde_json::to_string_pretty(&capsule)?);
                    }
                    None => {
                        println!("(not found)");
                    }
                }
            }
        },
    }

    Ok(())
}
