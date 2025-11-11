#[cfg(feature = "phase4")]
use anyhow::anyhow;
use anyhow::Result;
#[cfg(feature = "modular_pipeline")]
use capsule_registry::modular_pipeline;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
#[cfg(feature = "phase4")]
use clap::{Args, ValueEnum};
use clap::{Parser, Subcommand};
#[cfg(feature = "phase4")]
use common::podms::ZoneId;
use common::CapsuleId;
#[cfg(any(
    feature = "pipeline_async",
    feature = "modular_pipeline",
    feature = "phase4"
))]
use common::Policy;
#[cfg(feature = "phase4")]
use csi_driver_rs::ProvisionRequest;
use nvram_sim::NvramLog;
use protocol_block::BlockView;
#[cfg(feature = "phase4")]
use protocol_csi::csi_provision_capsule;
#[cfg(feature = "phase4")]
use protocol_fuse::mount_fuse_view;
#[cfg(feature = "phase4")]
use protocol_nfs::phase4::export_nfs_view;
use protocol_nfs::NfsView;
#[cfg(feature = "phase4")]
use protocol_nvme::project_nvme_view;
#[cfg(feature = "phase4")]
use scaling::MeshNode;
use std::fs;
use std::io::{self, Write};
#[cfg(feature = "phase4")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(feature = "phase4")]
use std::sync::Arc;
use std::sync::Once;
#[cfg(feature = "modular_pipeline")]
use tokio::runtime::Runtime as TokioRuntime;
#[cfg(feature = "phase4")]
use tokio::runtime::Runtime;
use tracing_subscriber::EnvFilter;
#[cfg(feature = "phase4")]
use uuid::Uuid;

const NVRAM_PATH: &str = "space.nvram";
const NFS_NAMESPACE_FILE: &str = "space.nfs.json";
const BLOCK_METADATA_FILE: &str = "space.block.json";

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
    #[command(subcommand)]
    command: Commands,
}

#[cfg(feature = "phase4")]
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Phase4View {
    Nvme,
    Nfs,
    Fuse,
    Csi,
}

#[cfg(feature = "phase4")]
#[derive(Clone, Debug, Args)]
struct ProjectArgs {
    /// Protocol view to project.
    #[arg(long, value_enum)]
    view: Phase4View,
    /// Capsule UUID to materialize.
    #[arg(long)]
    id: String,
    /// YAML policy file driving the projection.
    #[arg(long)]
    policy_file: String,
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

fn open_registry_and_nvram() -> Result<(CapsuleRegistry, NvramLog)> {
    let registry = CapsuleRegistry::new();
    let nvram = NvramLog::open(NVRAM_PATH)?;
    Ok((registry, nvram))
}

#[cfg(feature = "modular_pipeline")]
fn build_modular_pipeline_handle(
    registry: CapsuleRegistry,
) -> Result<(modular_pipeline::RegistryPipelineHandle, TokioRuntime)> {
    let handle = modular_pipeline::registry_pipeline_from_env(NVRAM_PATH, registry)?;
    let runtime = TokioRuntime::new()?;
    Ok((handle, runtime))
}

#[cfg(feature = "modular_pipeline")]
fn modular_write_capsule(data: &[u8]) -> Result<CapsuleId> {
    let registry = CapsuleRegistry::new();
    let (mut handle, runtime) = build_modular_pipeline_handle(registry)?;
    runtime.block_on(async { handle.write_capsule(data, &Policy::default()).await })
}

#[cfg(feature = "modular_pipeline")]
fn modular_read_capsule(id: CapsuleId) -> Result<Vec<u8>> {
    let registry = CapsuleRegistry::new();
    let (handle, runtime) = build_modular_pipeline_handle(registry)?;
    runtime.block_on(async { handle.read_capsule(id).await })
}

fn run_nfs_command(command: NfsCommands) -> Result<()> {
    let (registry, nvram) = open_registry_and_nvram()?;
    let nfs = NfsView::open(registry, nvram, NFS_NAMESPACE_FILE)?;

    match command {
        NfsCommands::Mkdir { path } => {
            nfs.mkdir(&path)?;
            println!("Created directory tree: {}", path);
        }
        NfsCommands::Write { path, file } => {
            let data = fs::read(&file)?;
            let capsule = nfs.write_file(&path, data)?;
            println!(
                "Wrote {} (capsule {}) from {}",
                path,
                capsule.as_uuid(),
                file
            );
        }
        NfsCommands::Read { path } => {
            let data = nfs.read_file(&path)?;
            io::stdout().write_all(&data)?;
        }
        NfsCommands::List { path } => {
            let entries = nfs.list_directory(&path)?;
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
            nfs.delete(&path)?;
            println!("Deleted {}", path);
        }
        NfsCommands::Metadata { path } => {
            let entry = nfs.metadata(&path)?;
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

fn run_block_command(command: BlockCommands) -> Result<()> {
    let (registry, nvram) = open_registry_and_nvram()?;
    let block = BlockView::open(registry, nvram, BLOCK_METADATA_FILE)?;

    match command {
        BlockCommands::Create {
            name,
            size,
            block_size,
        } => {
            let volume = if let Some(block_size) = block_size {
                block.create_volume_with_block_size(&name, size, block_size)?
            } else {
                block.create_volume(&name, size)?
            };
            println!(
                "Created volume {} (size={} bytes, block_size={})",
                volume.name(),
                volume.size(),
                volume.block_size()
            );
        }
        BlockCommands::Delete { name } => {
            block.delete_volume(&name)?;
            println!("Deleted volume {}", name);
        }
        BlockCommands::List => {
            let volumes = block.list_volumes();
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
            let volume = block.volume(&name)?;
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
            let data = block.read(&name, offset, length)?;
            io::stdout().write_all(&data)?;
        }
        BlockCommands::Write { name, offset, file } => {
            let data = fs::read(&file)?;
            block.write(&name, offset, &data)?;
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
}

#[cfg(feature = "phase4")]
fn load_policy_file(path: &str) -> Result<Policy> {
    let text = fs::read_to_string(path)?;
    serde_yaml::from_str(&text).map_err(|err| anyhow!(err))
}

#[cfg(feature = "phase4")]
fn handle_project_command(args: ProjectArgs) -> Result<()> {
    let ProjectArgs {
        view,
        id,
        policy_file,
    } = args;
    let uuid = Uuid::parse_str(&id).map_err(|err| anyhow!(err))?;
    let capsule_id = CapsuleId::from_uuid(uuid);
    let policy = load_policy_file(&policy_file)?;
    let registry = Arc::new(CapsuleRegistry::new());

    let rt = Runtime::new()?;
    let mesh = rt.block_on(async {
        MeshNode::new(
            ZoneId::Metro {
                name: "local".into(),
            },
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .await
    })?;
    let mesh = Arc::new(mesh);

    rt.block_on({
        let mesh = mesh.clone();
        let registry = Arc::clone(&registry);
        async move {
            match view {
                Phase4View::Nvme => {
                    project_nvme_view(capsule_id, &policy, mesh.as_ref(), registry.as_ref())
                        .await?;
                }
                Phase4View::Nfs => {
                    export_nfs_view(capsule_id, &policy, mesh.as_ref(), registry.as_ref()).await?;
                }
                Phase4View::Fuse => {
                    mount_fuse_view(
                        capsule_id,
                        &policy,
                        mesh.as_ref(),
                        "/tmp/space",
                        registry.as_ref(),
                    )
                    .await?;
                }
                Phase4View::Csi => {
                    let req = ProvisionRequest::from_capsule(&capsule_id.as_uuid().to_string());
                    csi_provision_capsule(req, &policy, mesh.as_ref(), registry.as_ref()).await?;
                }
            }

            tracing::info!(view = ?view, capsule = %capsule_id.as_uuid(), "projected view");
            Ok::<(), anyhow::Error>(())
        }
    })?;

    Ok(())
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Commands::Create {
            file,
            #[cfg(feature = "modular_pipeline")]
            modular,
        } => {
            let data = fs::read(&file)?;
            #[cfg(feature = "modular_pipeline")]
            if modular {
                let id = modular_write_capsule(&data)?;
                println!("Capsule created: {}", id.as_uuid());
                println!("Size: {} bytes", data.len());
                return Ok(());
            }

            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::new(registry, nvram);
            #[cfg(feature = "pipeline_async")]
            let id = {
                let policy = Policy::default();
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(pipeline.write_capsule_with_policy_async(&data, &policy))?
            };
            #[cfg(not(feature = "pipeline_async"))]
            let id = pipeline.write_capsule(&data)?;
            println!("Capsule created: {}", id.as_uuid());
            println!("Size: {} bytes", data.len());
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
                let data = modular_read_capsule(id)?;
                io::stdout().write_all(&data)?;
                return Ok(());
            }

            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::new(registry, nvram);
            let data = pipeline.read_capsule(id)?;
            io::stdout().write_all(&data)?;
        }
        Commands::List => {
            let registry = CapsuleRegistry::new();
            let capsule_ids = registry.list_capsules();

            if capsule_ids.is_empty() {
                println!("(no capsules)");
            } else {
                println!("Capsule ID\tSize (bytes)\tSegments");
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

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async { server.run().await })?;
        }
        #[cfg(feature = "phase4")]
        Commands::Project(args) => {
            handle_project_command(args)?;
        }
        Commands::Nfs { command } => {
            run_nfs_command(command)?;
        }
        Commands::Block { command } => {
            run_block_command(command)?;
        }
    }

    Ok(())
}
