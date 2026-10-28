use anyhow::{anyhow, Context, Result};
use capsule_registry::modular_pipeline;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};

use clap::{Parser, Subcommand};

use common::CapsuleId;
use common::{Policy, SegmentId};

use nvram_sim::NvramLog;

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;

use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const NVRAM_PATH: &str = "space.nvram";
const REGISTRY_DB_PATH: &str = "space.db";
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

#[derive(Subcommand)]
enum Commands {
    /// Create a new capsule from data
    Create {
        /// Input file path
        #[arg(short, long)]
        file: String,
    },
    /// Store a local file as a capsule without running it through the pipeline.
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
    },
    /// List all capsules
    List,
    /// Start S3-compatible HTTP server
    ServeS3 {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
}

fn load_policy_file(path: &PathBuf) -> Result<Policy> {
    let text = fs::read_to_string(path)?;
    serde_yaml::from_str(&text).map_err(|err| anyhow!(err))
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
        Commands::Create { file } => {
            let data = fs::read(&file)?;
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
            let registry = Arc::new(registry);
            let nvram = Arc::new(nvram);
            let seg_id: SegmentId = registry.alloc_segment()?;
            nvram.append(seg_id, &data)?;
            registry.create_capsule_with_segments(
                capsule_id,
                data.len() as u64,
                vec![seg_id],
                policy.clone(),
            )?;

            println!("{}", capsule_id.as_uuid());
        }
        Commands::Read { capsule_id } => {
            let uuid = capsule_id.parse()?;
            let id = CapsuleId::from_uuid(uuid);
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
        Commands::ServeS3 { port } => {
            use protocol_s3::{server::S3Server, S3View};

            println!("Starting SPACE S3 Protocol View...");

            let registry = CapsuleRegistry::new();
            let s3_view = if let Ok(storage_root) = std::env::var("SPACE_STORAGE_ROOT") {
                let storage_root = PathBuf::from(storage_root);
                let storage = storage::AutoFsBackend::open(&storage_root).await?;
                let handle =
                    modular_pipeline::registry_pipeline_from_fs_backend(storage, registry)?;
                S3View::new_modular(handle)
            } else {
                let handle = modular_pipeline::registry_pipeline_from_env(NVRAM_PATH, registry)?;
                S3View::new_modular(handle)
            };

            let server = S3Server::new(s3_view, port);
            server.run().await?;
        }
    }

    Ok(())
}
