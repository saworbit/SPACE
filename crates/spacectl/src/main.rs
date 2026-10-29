use anyhow::{anyhow, Context, Result};
use capsule_registry::modular_pipeline;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};

use clap::{Parser, Subcommand};

use common::CapsuleId;
use common::Policy;
use futures::StreamExt;

use nvram_sim::NvramLog;

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Once;

use tracing_subscriber::EnvFilter;

const NVRAM_PATH: &str = "space.nvram";
const REGISTRY_DB_PATH: &str = "space.db";
const LIST_PAGE_SIZE: usize = 256;

/// Logs go to stderr so they never mix with capsule bytes or IDs on stdout.
/// Colour is used only when stderr is a terminal.
fn init_tracing(default_level: &str) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
        let format = std::env::var("SPACE_LOG_FORMAT").unwrap_or_else(|_| "compact".to_string());

        if format.eq_ignore_ascii_case("json") {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter.clone())
                .with_writer(io::stderr)
                .with_target(true)
                .json()
                .flatten_event(true)
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(io::stderr)
                .with_ansi(io::stderr().is_terminal())
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

/// sled allows one process per store, so a second spacectl (or a running
/// serve-s3) in the same directory fails to open it.
fn open_registry() -> Result<CapsuleRegistry> {
    CapsuleRegistry::open(REGISTRY_DB_PATH).map_err(|err| {
        let locked = err
            .chain()
            .any(|cause| cause.to_string().contains("could not acquire lock"));
        if locked {
            anyhow!(
                "{REGISTRY_DB_PATH} is locked by another process. Only one spacectl \
                 command, or one serve-s3, can use a store at a time."
            )
        } else {
            err.context(format!("failed to open {REGISTRY_DB_PATH}"))
        }
    })
}

fn open_registry_and_nvram() -> Result<(CapsuleRegistry, NvramLog)> {
    let registry = open_registry()?;
    let nvram = NvramLog::open(NVRAM_PATH)?;
    Ok((registry, nvram))
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new capsule from data
    Create {
        /// Input file path
        #[arg(short, long)]
        file: String,
        /// YAML policy file. Without one, the capsule is encrypted when
        /// SPACE_MASTER_KEY is set and stored unencrypted otherwise.
        #[arg(long)]
        policy_file: Option<PathBuf>,
    },
    /// Read capsule contents
    Read {
        /// Capsule UUID
        capsule_id: String,
    },
    /// List all capsules
    List,
    /// Delete a capsule. Segments no other capsule shares are freed. An S3
    /// key that pointed at it then reads as missing.
    Delete {
        /// Capsule UUID
        capsule_id: String,
    },
    /// Start S3-compatible HTTP server
    ServeS3 {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
        /// Address to listen on. The server has no authentication, so it
        /// listens on loopback unless told otherwise.
        #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
        bind: IpAddr,
    },
}

fn load_policy_file(path: &PathBuf) -> Result<Policy> {
    let text = fs::read_to_string(path)?;
    serde_yaml::from_str(&text).map_err(|err| anyhow!(err))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let default_level = match cli.command {
        Commands::ServeS3 { .. } => "info",
        _ => "warn",
    };
    init_tracing(default_level);

    match cli.command {
        Commands::Create { file, policy_file } => {
            // Read one segment at a time rather than the whole file (#99).
            let input = tokio::fs::File::open(&file)
                .await
                .with_context(|| format!("can't open {file}"))?;
            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::open(registry, nvram)?;
            let policy = match policy_file {
                Some(path) => load_policy_file(&path)
                    .with_context(|| format!("invalid policy file {}", path.display()))?,
                None => pipeline.default_policy().await,
            };
            let (id, size) = pipeline.write_reader(input, &policy).await?;
            println!("Capsule created: {}", id.as_uuid());
            println!("Size: {size} bytes");
        }
        Commands::Read { capsule_id } => {
            let uuid = capsule_id.parse()?;
            let id = CapsuleId::from_uuid(uuid);
            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::open(registry, nvram)?;
            let mut stream = pipeline.read_stream(id).await?;
            let mut stdout = io::stdout().lock();
            while let Some(chunk) = stream.next().await {
                stdout.write_all(&chunk?)?;
            }
            stdout.flush()?;
        }
        Commands::Delete { capsule_id } => {
            let id = CapsuleId::from_uuid(capsule_id.parse()?);
            let (registry, nvram) = open_registry_and_nvram()?;
            let pipeline = WritePipeline::open(registry, nvram)?;
            pipeline.delete_capsule(id).await?;
            println!("Capsule deleted: {}", id.as_uuid());
        }
        Commands::List => {
            let registry = open_registry()?;
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
        Commands::ServeS3 { port, bind } => {
            use protocol_s3::{server::S3Server, S3View};

            println!("Starting SPACE S3 Protocol View...");

            let registry = open_registry()?;
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

            let server = S3Server::new(s3_view, SocketAddr::new(bind, port));
            server.run().await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_s3_listens_on_loopback_by_default() {
        let cli = Cli::try_parse_from(["spacectl", "serve-s3"]).unwrap();
        let Commands::ServeS3 { port, bind } = cli.command else {
            panic!("expected serve-s3");
        };
        assert_eq!(port, 8080);
        assert_eq!(bind, IpAddr::V4(Ipv4Addr::LOCALHOST));

        let cli = Cli::try_parse_from(["spacectl", "serve-s3", "--bind", "0.0.0.0"]).unwrap();
        let Commands::ServeS3 { bind, .. } = cli.command else {
            panic!("expected serve-s3");
        };
        assert_eq!(bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }
}
