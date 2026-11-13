use anyhow::{anyhow, bail, Context, Result};
use capsule_registry::modular_pipeline::{self, RegistryPipelineHandle};
use capsule_registry::CapsuleRegistry;

use clap::{Parser, Subcommand};

use common::scrub::{ScrubConfig, ScrubKind, ScrubResult};
use common::{CapsuleId, Policy, PolicyFile, SegmentId};
use futures::StreamExt;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Once};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const NVRAM_PATH: &str = "space.nvram";
const REGISTRY_DB_PATH: &str = "space.db";
const JOURNAL_PATH: &str = "space.journal";
const LIST_PAGE_SIZE: usize = 256;
/// The shortest capsule id prefix `read` and `delete` accept.
const MIN_PREFIX: usize = 4;

/// `spacectl --version`: the build, and the on-disk format it reads and
/// writes (#168).
static VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{} (on-disk format {})",
        env!("CARGO_PKG_VERSION"),
        common::format::FORMAT_VERSION
    )
});

/// Print a line of command output. Unlike `println!`, a stdout whose reader
/// has gone away is an error rather than a panic, and `main` ends quietly
/// on it (#167).
macro_rules! say {
    ($($arg:tt)*) => {
        writeln!(io::stdout(), $($arg)*)?
    };
}

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
#[command(version = VERSION.as_str())]
#[command(
    after_help = "Every command uses the store in --store, the current directory unless \
                  told otherwise: space.db and space.journal, with segments under \
                  SPACE_STORAGE_ROOT when that's set and in space.nvram otherwise. Only \
                  create and serve-s3 create a store."
)]
struct Cli {
    /// The store's directory, which holds space.db, space.journal and
    /// space.nvram.
    #[arg(
        long,
        global = true,
        env = "SPACE_STORE_DIR",
        default_value = ".",
        value_name = "DIR"
    )]
    store: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

/// Whether a command may create the store it opens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    /// `create` and `serve-s3`.
    OrCreate,
    /// Everything else: a missing store is an error, not a new empty one
    /// that reads as if the data were gone (#166).
    Existing,
}

/// sled allows one process per store, so a second spacectl (or a running
/// serve-s3) in the same directory fails to open it.
fn open_registry(dir: &Path, open: Open) -> Result<CapsuleRegistry> {
    match open {
        Open::OrCreate => fs::create_dir_all(dir)
            .with_context(|| format!("can't create the store directory {}", dir.display()))?,
        Open::Existing => {
            if !dir.join(JOURNAL_PATH).exists() && !dir.join(REGISTRY_DB_PATH).exists() {
                bail!(
                    "no SPACE store in {} (looked for {JOURNAL_PATH}); use --store or cd \
                     into the store",
                    dir.display()
                );
            }
        }
    }
    CapsuleRegistry::open(dir.join(REGISTRY_DB_PATH)).map_err(|err| {
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

/// `SPACE_STORAGE_ROOT`, if it's set. Set but empty is an error: an empty
/// path is the current directory, which would quietly become a storage
/// root (#165).
fn storage_root() -> Result<Option<PathBuf>> {
    match std::env::var_os("SPACE_STORAGE_ROOT") {
        None => Ok(None),
        Some(root) if root.is_empty() => bail!(
            "SPACE_STORAGE_ROOT is set but empty. Unset it to keep segments in space.nvram, \
             or set it to a directory."
        ),
        Some(root) => Ok(Some(PathBuf::from(root))),
    }
}

/// The store's pipeline, as `serve-s3` opens it: segments under
/// `SPACE_STORAGE_ROOT` when that's set, in `space.nvram` otherwise. Every
/// command that touches segments opens it this way, so they all use the
/// store the capsule journal is paired with.
///
/// A journal that already holds records belongs to the store its segments
/// are in. When that place has no store yet, opening it would start a new
/// one, so it's refused before anything is created there (#165).
async fn open_store(dir: &Path, registry: CapsuleRegistry) -> Result<RegistryPipelineHandle> {
    let nvram = dir.join(NVRAM_PATH);
    let established = !registry.journal_is_empty();
    match storage_root()? {
        Some(root) => {
            if established && !root.join("store").exists() {
                let fix = if nvram.exists() {
                    "It keeps its segments in space.nvram, so unset SPACE_STORAGE_ROOT."
                } else {
                    "Set SPACE_STORAGE_ROOT to the directory its segments are in."
                };
                bail!(
                    "SPACE_STORAGE_ROOT is '{}', which holds no store, but the store in {} \
                     already has records. {fix}",
                    root.display(),
                    dir.display()
                );
            }
            let storage = storage::AutoFsBackend::open(&root).await?;
            modular_pipeline::registry_pipeline_from_fs_backend(storage, registry).with_context(
                || {
                    format!(
                        "couldn't open the segments under SPACE_STORAGE_ROOT '{}'",
                        root.display()
                    )
                },
            )
        }
        None => {
            if established && !nvram.exists() {
                bail!(
                    "the store in {} has records but no {NVRAM_PATH}, so its segments are \
                     under a storage root. Set SPACE_STORAGE_ROOT to that directory.",
                    dir.display()
                );
            }
            modular_pipeline::registry_pipeline_from_env(&nvram, registry)
                .with_context(|| format!("couldn't open {}", nvram.display()))
        }
    }
}

/// The capsule an argument names: its full id, or a prefix of one as `list`
/// prints them that only one capsule has (#168).
fn resolve_capsule(registry: &CapsuleRegistry, arg: &str) -> Result<CapsuleId> {
    if let Ok(uuid) = arg.parse::<uuid::Uuid>() {
        let id = CapsuleId::from_uuid(uuid);
        if registry.get_capsule(id)?.is_none() {
            bail!("no capsule {uuid} in this store");
        }
        return Ok(id);
    }
    let prefix = arg.to_ascii_lowercase();
    if prefix.len() < MIN_PREFIX || !prefix.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        bail!(
            "'{arg}' is not a capsule id, or a prefix of one at least {MIN_PREFIX} characters \
             long"
        );
    }
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let page = registry.list_capsules(LIST_PAGE_SIZE, cursor)?;
        let Some(last) = page.last().copied() else {
            break;
        };
        cursor = Some(last);
        found.extend(
            page.into_iter()
                .filter(|id| id.as_uuid().to_string().starts_with(&prefix)),
        );
    }
    match found.as_slice() {
        [] => bail!("no capsule in this store has an id starting with {arg}"),
        [id] => Ok(*id),
        _ => bail!(
            "{arg} could be any of {} capsules; give more of the id",
            found.len()
        ),
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
        /// YAML policy file setting any of compression, dedupe, encryption
        /// and layout (docs/capsule.md). Unknown keys are errors. Settings
        /// it leaves out keep the default: the capsule is encrypted when
        /// SPACE_MASTER_KEY is set. Without it, the capsule is stored
        /// unencrypted, unless the store has a key, which refuses it.
        #[arg(long)]
        policy_file: Option<PathBuf>,
    },
    /// Read capsule contents
    Read {
        /// Capsule ID, or a prefix of one that only one capsule has
        capsule_id: String,
    },
    /// List all capsules
    List,
    /// Delete a capsule. Segments no other capsule shares are freed. An S3
    /// key that pointed at it then reads as missing.
    Delete {
        /// Capsule ID, or a prefix of one that only one capsule has
        capsule_id: String,
    },
    /// Remove segments no capsule references, such as those an interrupted
    /// write left behind. In space.nvram their bytes stay until `compact`.
    Gc,
    /// Reclaim space by writing new generations of space.nvram, without the
    /// segments no capsule references, and of space.journal, with only the
    /// live capsules, buckets and keys.
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
        /// Address to listen on. The server has no authentication, so
        /// without one it listens on loopback: on 127.0.0.1 and on ::1, for
        /// clients that resolve localhost to ::1 first.
        #[arg(long)]
        bind: Option<IpAddr>,
        /// Seconds between passes that reclaim space: compacting space.nvram
        /// once a quarter of it is reclaimable, or deleting unreferenced
        /// segments under SPACE_STORAGE_ROOT, then compacting space.journal
        /// once a quarter of it is dead. The first runs at startup. 0 turns
        /// them off.
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

async fn bind_to(addr: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("couldn't listen on {addr}; try another --port"))
}

/// serve-s3's listeners: one on `bind` when it's given, or one on each
/// loopback address (#158). Listening on 127.0.0.1 alone made clients that
/// resolve localhost to ::1 first wait for that attempt to fail on every
/// connection, about 2 s on Windows. A host with no IPv6 gets 127.0.0.1
/// alone, and a warning.
async fn listen(bind: Option<IpAddr>, port: u16) -> Result<Vec<TcpListener>> {
    if let Some(ip) = bind {
        return Ok(vec![bind_to(SocketAddr::new(ip, port)).await?]);
    }
    let v4 = bind_to(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)).await?;
    // With --port 0, ::1 takes the port the OS picked for 127.0.0.1.
    let v6 = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), v4.local_addr()?.port());
    match TcpListener::bind(v6).await {
        Ok(listener) => Ok(vec![v4, listener]),
        // Something else answers localhost clients there.
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            Err(err).with_context(|| format!("couldn't listen on {v6}; try another --port"))
        }
        Err(err) => {
            warn!("listening on 127.0.0.1 only: couldn't listen on {v6}: {err}");
            Ok(vec![v4])
        }
    }
}

/// One line saying what serve-s3 serves and how (#170), and a warning when
/// it's reachable from other hosts.
fn log_startup(
    addrs: &[SocketAddr],
    dir: &Path,
    store: &RegistryPipelineHandle,
    reclaim_interval: u64,
    scrub: ScrubArgs,
) {
    let absolute = |path: &Path| {
        std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    };
    let addr = addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let segments = match storage_root() {
        Ok(Some(root)) => absolute(&root),
        _ => absolute(&dir.join(NVRAM_PATH)),
    };
    let encryption = if store.default_policy().encryption.is_enabled() {
        "on"
    } else {
        "off"
    };
    // Names the store's key, so a log shows which one a server ran with.
    let key = store.store_key().map(|key| key.fingerprint_hex());
    let reclaim = match reclaim_interval {
        0 => "off".to_string(),
        secs => format!("{secs}s"),
    };
    let scrub = match scrub.config() {
        Some(config) => format!(
            "light:{}s,deep:{}s",
            config.light_interval.as_secs(),
            config.deep_interval.as_secs()
        ),
        None => "off".to_string(),
    };
    info!(
        addr = %addr,
        store = %absolute(dir),
        segments = %segments,
        encryption = %encryption,
        key = key.as_deref().map(tracing::field::display),
        reclaim = %reclaim,
        scrub = %scrub,
        "listening"
    );
    for addr in addrs.iter().filter(|addr| !addr.ip().is_loopback()) {
        warn!(
            %addr,
            "listening beyond loopback with no authentication: anyone who can reach this \
             address can read, change and delete every object"
        );
    }
}

/// A policy file's settings over `base`. Unknown keys and settings nothing
/// implements are errors (#161, #162).
fn load_policy_file(path: &PathBuf, base: Policy) -> Result<Policy> {
    let text = fs::read_to_string(path)?;
    let file: PolicyFile = serde_norway::from_str(&text).map_err(|err| anyhow!(err))?;
    Ok(file.apply(base)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let default_level = match cli.command {
        Commands::ServeS3 { .. } => "info",
        _ => "warn",
    };
    init_tracing(default_level);

    match run(cli).await {
        // A reader that stopped early, such as `head`, isn't a failure.
        Err(err) if broken_pipe(&err) => Ok(()),
        result => result,
    }
}

/// Whether an error is stdout's reader having gone away (#167).
fn broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|err| err.kind() == io::ErrorKind::BrokenPipe)
    })
}

async fn run(cli: Cli) -> Result<()> {
    let dir = cli.store.as_path();
    match cli.command {
        Commands::Create { file, policy_file } => {
            // Read one segment at a time rather than the whole file (#99).
            let input = tokio::fs::File::open(&file)
                .await
                .with_context(|| format!("can't open {file}"))?;
            let store = open_store(dir, open_registry(dir, Open::OrCreate)?).await?;
            let policy = match policy_file {
                Some(path) => load_policy_file(&path, store.default_policy())
                    .with_context(|| format!("invalid policy file {}", path.display()))?,
                None => store.default_policy(),
            };
            let store = Arc::new(TokioMutex::new(store));
            let (id, size) = modular_pipeline::write_from_reader(&store, input, &policy).await?;
            say!("Capsule created: {}", id.as_uuid());
            say!("Size: {size} bytes");
        }
        Commands::Read { capsule_id } => {
            let registry = open_registry(dir, Open::Existing)?;
            let id = resolve_capsule(&registry, &capsule_id)?;
            let store = open_store(dir, registry).await?;
            let mut stream = store.read_capsule_stream(id).await?;
            let mut stdout = io::stdout().lock();
            while let Some(chunk) = stream.next().await {
                stdout.write_all(&chunk?)?;
            }
            stdout.flush()?;
        }
        Commands::Delete { capsule_id } => {
            let registry = open_registry(dir, Open::Existing)?;
            let id = resolve_capsule(&registry, &capsule_id)?;
            let mut store = open_store(dir, registry).await?;
            store.delete_capsule(id).await?;
            say!("Capsule deleted: {}", id.as_uuid());
        }
        Commands::Gc => {
            let mut store = open_store(dir, open_registry(dir, Open::Existing)?).await?;
            let collected = store.garbage_collect().await?;
            say!(
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
                    say!(
                        "{} of space.nvram can be reclaimed; run `spacectl compact`",
                        plural(reclaimable, "byte", "bytes")
                    );
                }
            }
        }
        Commands::Compact => {
            let store = open_store(dir, open_registry(dir, Open::Existing)?).await?;
            match store.data_log() {
                Some(log) => {
                    let before = log.size();
                    match log.compact()? {
                        0 => say!("space.nvram: nothing to reclaim"),
                        reclaimed => say!(
                            "space.nvram: reclaimed {} of {before}, now generation {}",
                            plural(reclaimed, "byte", "bytes"),
                            log.file_header().generation
                        ),
                    }
                }
                None => say!(
                    "Segments under SPACE_STORAGE_ROOT are files of their own and need no \
                     compaction; `spacectl gc` deletes the unreferenced ones"
                ),
            }
            match store.registry().compact_journal()? {
                None => say!("space.journal: nothing to reclaim"),
                Some(done) => say!(
                    "space.journal: reclaimed {} of {}, now generation {}",
                    plural(done.reclaimed(), "byte", "bytes"),
                    done.before,
                    done.generation
                ),
            }
        }
        Commands::List => {
            let registry = open_registry(dir, Open::Existing)?;
            let mut cursor = None;
            let mut printed = false;

            loop {
                let capsule_ids = registry.list_capsules(LIST_PAGE_SIZE, cursor)?;
                if capsule_ids.is_empty() {
                    break;
                }

                if !printed {
                    say!("Capsule ID\tSize (bytes)\tSegments");
                    printed = true;
                }

                cursor = capsule_ids.last().copied();

                for id in capsule_ids {
                    match registry.lookup(id) {
                        Ok(capsule) => {
                            say!(
                                "{}\t{:>12}\t{:>3}",
                                capsule.id.as_uuid(),
                                capsule.size,
                                capsule.segments.len()
                            );
                        }
                        Err(err) => {
                            say!("{} \t<error: {}>", id.as_uuid(), err);
                        }
                    }
                }
            }

            if !printed {
                say!("(no capsules)");
            }
        }
        Commands::Scrub { deep } => {
            let registry = open_registry(dir, Open::Existing)?;
            let store = open_store(dir, registry.clone()).await?;
            let (kind, name) = if deep {
                (ScrubKind::Deep, "Deep")
            } else {
                (ScrubKind::Light, "Light")
            };
            let report = store.scrub_all(kind).await;
            say!(
                "{name} scrub: checked {} ({}) in {:.1?}",
                plural(report.segments_checked as u64, "segment", "segments"),
                plural(report.bytes_checked, "byte", "bytes"),
                report.duration
            );
            if report.segments_skipped > 0 {
                say!(
                    "Skipped {}: set SPACE_MASTER_KEY to check encrypted segments' MACs",
                    plural(report.segments_skipped as u64, "segment", "segments")
                );
            }
            if report.errors.is_empty() {
                say!("All healthy");
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
                    say!("{}", describe_scrub_error(error, &owners));
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

            // Listen before opening the store, so a port that's in use
            // doesn't leave a new store behind (#166).
            let listeners = listen(bind, port).await?;
            let registry = open_registry(dir, Open::OrCreate)?;
            let store = open_store(dir, registry).await?;
            // Without its key, an encrypted store would refuse every PUT.
            store.check_writable()?;
            let addrs = listeners
                .iter()
                .map(TcpListener::local_addr)
                .collect::<io::Result<Vec<_>>>()?;
            log_startup(&addrs, dir, &store, reclaim_interval, scrub);
            let s3_view = S3View::new_modular(store);
            let reclaim_every =
                (reclaim_interval > 0).then(|| Duration::from_secs(reclaim_interval));
            let server = S3Server::new(s3_view, listeners)
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
    use common::format::CapsuleManifest;

    #[test]
    fn capsules_resolve_by_full_id_or_unique_prefix() {
        let dir = std::env::temp_dir().join(format!("spacectl_resolve_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = CapsuleRegistry::open(dir.join(REGISTRY_DB_PATH)).unwrap();
        let ids = [
            "69b97199-3eec-4acd-adb2-e6ee831001c8",
            "69b97299-0000-4000-8000-000000000000",
            "e49ef47a-1de2-4f9c-a750-32305ba380a6",
        ];
        for id in ids {
            registry
                .create_capsule(CapsuleManifest {
                    capsule_id: CapsuleId::from_uuid(id.parse().unwrap()),
                    created_at: 0,
                    size: 0,
                    digest: [0; 32],
                    dedupe: true,
                    requires_encryption: false,
                    deduped_bytes: 0,
                    policy: Policy::default(),
                    entries: Vec::new(),
                })
                .unwrap();
        }
        let resolve = |arg: &str| {
            resolve_capsule(&registry, arg)
                .map(|id| id.as_uuid().to_string())
                .map_err(|err| err.to_string())
        };

        assert_eq!(resolve(ids[0]).unwrap(), ids[0]);
        assert_eq!(resolve(&ids[0].to_uppercase()).unwrap(), ids[0]);
        assert_eq!(resolve("69b971").unwrap(), ids[0]);
        assert_eq!(resolve("E49E").unwrap(), ids[2]);
        assert_eq!(resolve("e49ef47a-1de2").unwrap(), ids[2]);
        assert!(resolve("69b9")
            .unwrap_err()
            .contains("69b9 could be any of 2 capsules"));
        assert!(resolve("0000")
            .unwrap_err()
            .contains("no capsule in this store has an id starting with 0000"));
        for bad in ["69b", "not-a-uuid", ""] {
            assert!(
                resolve(bad)
                    .unwrap_err()
                    .contains(&format!("'{bad}' is not a capsule id")),
                "{bad}"
            );
        }
        let missing = "00000000-0000-0000-0000-000000000001";
        assert_eq!(
            resolve(missing).unwrap_err(),
            format!("no capsule {missing} in this store")
        );

        drop(registry);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_names_the_on_disk_format() {
        let err = Cli::try_parse_from(["spacectl", "--version"])
            .err()
            .unwrap();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        assert_eq!(
            err.to_string().trim(),
            format!(
                "spacectl {} (on-disk format {})",
                env!("CARGO_PKG_VERSION"),
                common::format::FORMAT_VERSION
            )
        );
    }

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
        assert_eq!(bind, None, "both loopback addresses");
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
        assert_eq!(bind, Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    }

    #[tokio::test]
    async fn serve_s3_listens_on_both_loopback_addresses_on_one_port() {
        let listeners = listen(None, 0).await.unwrap();
        let addrs: Vec<SocketAddr> = listeners.iter().map(|l| l.local_addr().unwrap()).collect();
        assert_eq!(addrs[0].ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_ne!(addrs[0].port(), 0);
        // A host without IPv6 gets 127.0.0.1 alone.
        if let Some(v6) = addrs.get(1) {
            assert_eq!(v6.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
            assert_eq!(v6.port(), addrs[0].port());
        }
        let host_has_ipv6 = std::net::TcpListener::bind("[::1]:0").is_ok();
        assert_eq!(addrs.len(), if host_has_ipv6 { 2 } else { 1 }, "{addrs:?}");

        let one = listen(Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), 0)
            .await
            .unwrap();
        assert_eq!(one.len(), 1);

        // A port taken on 127.0.0.1 is an error naming it.
        let port = addrs[0].port();
        let err = listen(None, port).await.unwrap_err().to_string();
        assert!(
            err.contains(&format!("couldn't listen on 127.0.0.1:{port}")),
            "{err}"
        );
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
