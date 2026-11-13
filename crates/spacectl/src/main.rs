use anyhow::{anyhow, bail, Context, Result};
use capsule_registry::modular_pipeline::{self, RegistryPipelineHandle};
use capsule_registry::CapsuleRegistry;

use clap::{Parser, Subcommand};

use common::scrub::{ScrubConfig, ScrubKind, ScrubResult};
use common::{CapsuleId, Policy, SegmentId};
use futures::StreamExt;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
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
#[command(
    after_help = "Every command uses the store in the current directory: space.db and \
                  space.journal, with segments under SPACE_STORAGE_ROOT when that's set \
                  and in space.nvram otherwise."
)]
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

/// The store's pipeline, as `serve-s3` opens it: segments under
/// `SPACE_STORAGE_ROOT` when that's set, in `space.nvram` otherwise. Every
/// command that touches segments opens it this way, so they all use the
/// store the capsule journal is paired with.
async fn open_store(registry: CapsuleRegistry) -> Result<RegistryPipelineHandle> {
    match std::env::var("SPACE_STORAGE_ROOT") {
        Ok(root) => {
            let storage = storage::AutoFsBackend::open(PathBuf::from(root)).await?;
            modular_pipeline::registry_pipeline_from_fs_backend(storage, registry)
        }
        Err(_) => modular_pipeline::registry_pipeline_from_env(NVRAM_PATH, registry),
    }
}

fn plural(n: impl Into<u64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
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
    /// Remove segments no capsule references, such as those an interrupted
    /// write left behind. In space.nvram their bytes stay until `compact`.
    Gc,
    /// Reclaim the bytes of segments no capsule references by writing a new
    /// generation of space.nvram.
    Compact,
    /// Check every segment's integrity now, and exit non-zero if any fails.
    /// This checks each segment record's checksums, in space.nvram or in its
    /// file under SPACE_STORAGE_ROOT.
    Scrub {
        /// Also check each segment's MAC (needs SPACE_MASTER_KEY) or content
        /// hash, which catches tampering as well as damage.
        #[arg(long)]
        deep: bool,
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
        /// Seconds between passes that reclaim space: compacting space.nvram
        /// once a quarter of it is reclaimable, or deleting unreferenced
        /// segments under SPACE_STORAGE_ROOT. The first runs at startup. 0
        /// turns them off.
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        reclaim_interval: u64,
        #[command(flatten)]
        scrub: ScrubArgs,
    },
}

/// When `serve-s3` scrubs the store. When each segment was last checked is
/// kept in space.db, so a restart only scrubs segments that are due.
#[derive(clap::Args, Debug, Clone, Copy, PartialEq, Eq)]
struct ScrubArgs {
    /// Seconds between light scrubs, which check each segment without a key.
    /// 0 turns scrubbing off.
    #[arg(long, default_value_t = 86_400, value_name = "SECONDS")]
    scrub_interval: u64,
    /// Seconds between deep scrubs, which also check MACs and content
    /// hashes. The first runs at startup, for segments that are due.
    #[arg(
        long,
        default_value_t = 604_800,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    deep_scrub_interval: u64,
    /// Most MiB per second scrubbing reads, so it leaves room for requests.
    /// 0 means no limit.
    #[arg(long, default_value_t = 64, value_name = "MIB")]
    scrub_rate: u64,
}

impl ScrubArgs {
    fn config(self) -> Option<ScrubConfig> {
        (self.scrub_interval > 0).then(|| ScrubConfig {
            light_interval: Duration::from_secs(self.scrub_interval),
            deep_interval: Duration::from_secs(self.deep_scrub_interval),
            max_bytes_per_sec: (self.scrub_rate > 0)
                .then(|| self.scrub_rate.saturating_mul(1024 * 1024)),
            ..ScrubConfig::default()
        })
    }
}

/// One line per failed segment, naming the capsules that hold it.
fn describe_scrub_error(
    result: &ScrubResult,
    owners: &HashMap<SegmentId, Vec<CapsuleId>>,
) -> String {
    let what = match result {
        ScrubResult::MetadataMismatch {
            expected_len,
            actual_len,
            ..
        } => format!("{actual_len} bytes stored, {expected_len} expected"),
        ScrubResult::ContentCorrupted { .. } => {
            "stored bytes don't match their checksum or content hash".to_string()
        }
        ScrubResult::MacMismatch { .. } => "MAC check failed".to_string(),
        ScrubResult::ReadError { error, .. } => format!("couldn't be read: {error}"),
        other => format!("{other:?}"),
    };
    let Some(segment) = result.segment() else {
        return what;
    };
    let held_by = match owners.get(&segment) {
        Some(ids) => {
            let ids: Vec<String> = ids.iter().map(|id| id.as_uuid().to_string()).collect();
            format!(" (capsule {})", ids.join(", "))
        }
        None => " (no capsule)".to_string(),
    };
    format!("segment {}: {what}{held_by}", segment.0)
}

fn load_policy_file(path: &PathBuf) -> Result<Policy> {
    let text = fs::read_to_string(path)?;
    serde_norway::from_str(&text).map_err(|err| anyhow!(err))
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
            let store = open_store(open_registry()?).await?;
            let policy = match policy_file {
                Some(path) => load_policy_file(&path)
                    .with_context(|| format!("invalid policy file {}", path.display()))?,
                None => store.default_policy(),
            };
            let store = Arc::new(TokioMutex::new(store));
            let (id, size) = modular_pipeline::write_from_reader(&store, input, &policy).await?;
            println!("Capsule created: {}", id.as_uuid());
            println!("Size: {size} bytes");
        }
        Commands::Read { capsule_id } => {
            let uuid = capsule_id.parse()?;
            let id = CapsuleId::from_uuid(uuid);
            let store = open_store(open_registry()?).await?;
            let mut stream = store.read_capsule_stream(id).await?;
            let mut stdout = io::stdout().lock();
            while let Some(chunk) = stream.next().await {
                stdout.write_all(&chunk?)?;
            }
            stdout.flush()?;
        }
        Commands::Delete { capsule_id } => {
            let id = CapsuleId::from_uuid(capsule_id.parse()?);
            let mut store = open_store(open_registry()?).await?;
            store.delete_capsule(id).await?;
            println!("Capsule deleted: {}", id.as_uuid());
        }
        Commands::Gc => {
            let mut store = open_store(open_registry()?).await?;
            let collected = store.garbage_collect().await?;
            println!(
                "Removed {}",
                plural(
                    collected as u64,
                    "unreferenced segment",
                    "unreferenced segments"
                )
            );
            if let Some(log) = store.data_log() {
                let reclaimable = log.reclaimable_bytes();
                if reclaimable > 0 {
                    println!(
                        "{} of space.nvram can be reclaimed; run `spacectl compact`",
                        plural(reclaimable, "byte", "bytes")
                    );
                }
            }
        }
        Commands::Compact => {
            let store = open_store(open_registry()?).await?;
            let Some(log) = store.data_log() else {
                bail!(
                    "segments under SPACE_STORAGE_ROOT are files of their own, so there is \
                     nothing to compact; `spacectl gc` deletes the unreferenced ones"
                );
            };
            let before = log.size();
            match log.compact()? {
                0 => println!("Nothing to reclaim"),
                reclaimed => {
                    println!(
                        "Reclaimed {} of {before}",
                        plural(reclaimed, "byte", "bytes")
                    );
                    println!(
                        "space.nvram is now generation {}",
                        log.file_header().generation
                    );
                }
            }
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
        Commands::Scrub { deep } => {
            let registry = open_registry()?;
            let store = open_store(registry.clone()).await?;
            let (kind, name) = if deep {
                (ScrubKind::Deep, "Deep")
            } else {
                (ScrubKind::Light, "Light")
            };
            let report = store.scrub_all(kind).await;
            println!(
                "{name} scrub: checked {} ({}) in {:.1?}",
                plural(report.segments_checked as u64, "segment", "segments"),
                plural(report.bytes_checked, "byte", "bytes"),
                report.duration
            );
            if report.segments_skipped > 0 {
                println!(
                    "Skipped {}: set SPACE_MASTER_KEY to check encrypted segments' MACs",
                    plural(report.segments_skipped as u64, "segment", "segments")
                );
            }
            if report.errors.is_empty() {
                println!("All healthy");
            } else {
                let mut owners: HashMap<SegmentId, Vec<CapsuleId>> = HashMap::new();
                let failed: HashSet<SegmentId> = report
                    .errors
                    .iter()
                    .filter_map(ScrubResult::segment)
                    .collect();
                for capsule in registry.capsules() {
                    for segment in capsule.segments.iter().filter(|s| failed.contains(s)) {
                        owners.entry(*segment).or_default().push(capsule.id);
                    }
                }
                for error in &report.errors {
                    println!("{}", describe_scrub_error(error, &owners));
                }
                let damaged =
                    report.mac_failures + report.content_failures + report.metadata_failures;
                if damaged > 0 {
                    bail!(
                        "{} failed integrity checks",
                        plural(damaged as u64, "segment", "segments")
                    );
                }
                bail!(
                    "{} couldn't be read",
                    plural(report.read_errors as u64, "segment", "segments")
                );
            }
        }
        Commands::ServeS3 {
            port,
            bind,
            reclaim_interval,
            scrub,
        } => {
            use protocol_s3::{server::S3Server, S3View};

            println!("Starting SPACE S3 Protocol View...");

            let s3_view = S3View::new_modular(open_store(open_registry()?).await?);
            let reclaim_every =
                (reclaim_interval > 0).then(|| Duration::from_secs(reclaim_interval));
            let server = S3Server::new(s3_view, SocketAddr::new(bind, port))
                .reclaim_every(reclaim_every)
                .scrub(scrub.config());
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
        let Commands::ServeS3 {
            port,
            bind,
            reclaim_interval,
            ..
        } = cli.command
        else {
            panic!("expected serve-s3");
        };
        assert_eq!(port, 8080);
        assert_eq!(bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(reclaim_interval, 300);

        let cli = Cli::try_parse_from(["spacectl", "serve-s3", "--reclaim-interval", "0"]).unwrap();
        let Commands::ServeS3 {
            reclaim_interval, ..
        } = cli.command
        else {
            panic!("expected serve-s3");
        };
        assert_eq!(reclaim_interval, 0);

        let cli = Cli::try_parse_from(["spacectl", "serve-s3", "--bind", "0.0.0.0"]).unwrap();
        let Commands::ServeS3 { bind, .. } = cli.command else {
            panic!("expected serve-s3");
        };
        assert_eq!(bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn serve_s3_scrubs_daily_and_deep_weekly_unless_told_otherwise() {
        let scrub = |args: &[&str]| {
            let cli = Cli::try_parse_from([&["spacectl", "serve-s3"][..], args].concat())?;
            let Commands::ServeS3 { scrub, .. } = cli.command else {
                panic!("expected serve-s3");
            };
            Ok::<_, clap::Error>(scrub.config())
        };

        let config = scrub(&[]).unwrap().unwrap();
        assert_eq!(config.light_interval, Duration::from_secs(86_400));
        assert_eq!(config.deep_interval, Duration::from_secs(604_800));
        assert_eq!(config.max_bytes_per_sec, Some(64 * 1024 * 1024));

        let config = scrub(&[
            "--scrub-interval",
            "60",
            "--deep-scrub-interval",
            "600",
            "--scrub-rate",
            "0",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.light_interval, Duration::from_secs(60));
        assert_eq!(config.deep_interval, Duration::from_secs(600));
        assert_eq!(config.max_bytes_per_sec, None);

        assert!(scrub(&["--scrub-interval", "0"]).unwrap().is_none());
        assert!(scrub(&["--deep-scrub-interval", "0"]).is_err());
    }
}
