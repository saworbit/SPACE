use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context, Result};
use cargo_metadata::{Metadata, MetadataCommand, PackageId};
use clap::{Parser, Subcommand};
use semver::{Version, VersionReq};
use serde::Serialize;
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const TRANSITIVE_LIMIT: usize = 50;

#[derive(Parser)]
#[command(author, version, about = "SPACE workspace helper tasks")]
struct Args {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Run formatting, checks, and security auditing in one pass.
    Audit {
        /// Skip running cargo test (useful for CI smoke runs).
        #[arg(long)]
        no_tests: bool,
    },
    /// Collect dependency drift information for scheduled runs.
    Drift {
        /// Optional path for the JSON report (defaults to target/xtask/drift-report.json).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Capture dependency graph artefacts for manual inspection.
    Graph,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        CommandKind::Audit { no_tests } => audit(no_tests),
        CommandKind::Drift { output } => drift(output),
        CommandKind::Graph => graph(),
    }
}

fn audit(no_tests: bool) -> Result<()> {
    println!("Running cargo fmt --check");
    run("cargo", ["fmt", "--all", "--", "--check"])?;

    println!("Running cargo check --all-targets");
    run("cargo", ["check", "--workspace", "--all-targets"])?;

    if !no_tests {
        println!("Running cargo test --all-targets");
        run("cargo", ["test", "--workspace", "--all-targets"])?;
    }

    println!("Validating feature allowlist");
    validate_feature_allowlist()?;

    println!("Capturing cargo tree report");
    capture_tree_report()?;

    println!("Running cargo audit");
    run("cargo", ["audit", "--deny", "warnings"])?;

    println!("Running cargo deny");
    run("cargo", ["deny", "check", "bans", "licenses", "sources"])?;

    println!("Running cargo bloat -p spacectl --crates --release");
    run(
        "cargo",
        ["bloat", "-p", "spacectl", "--crates", "--release"],
    )?;

    Ok(())
}

fn graph() -> Result<()> {
    let out_dir = Path::new("target").join("xtask");
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    println!("Writing cargo tree to target/xtask/cargo-tree.txt");
    let tree_bytes = run_capture(
        "cargo",
        [
            "tree",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--edges",
            "normal,build,dev",
            "--locked",
        ],
    )?;
    fs::write(out_dir.join("cargo-tree.txt"), tree_bytes)
        .context("failed to write cargo-tree.txt")?;

    println!("Attempting cargo deps (if installed) -> target/xtask/cargo-deps.txt");
    match Command::new("cargo")
        .args(["deps", "--all-deps", "--include-tests"])
        .output()
    {
        Ok(output) if output.status.success() => {
            fs::write(out_dir.join("cargo-deps.txt"), output.stdout)
                .context("failed to write cargo-deps.txt")?;
        }
        Ok(output) => {
            eprintln!(
                "cargo deps exited with {} – skipping graph capture (stderr: {})",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(err) => {
            eprintln!("cargo deps unavailable ({err}); install with `cargo install cargo-deps`");
        }
    }

    Ok(())
}

fn drift(output: Option<PathBuf>) -> Result<()> {
    let metadata = load_metadata().context("failed to load cargo metadata")?;
    let pins = load_version_pins().context("failed to load workspace version pins")?;

    let transitive_count = count_transitive(&metadata);
    let pin_mismatches = find_pin_mismatches(&metadata, &pins);
    let advisory_summary = collect_audit_summary().context("failed to run cargo audit --json")?;

    let summary = DriftSummary {
        timestamp: iso_timestamp()?,
        advisories: advisory_summary,
        transitive_count,
        transitive_limit: TRANSITIVE_LIMIT,
        pin_mismatches,
    };

    let output_path = output.unwrap_or_else(|| PathBuf::from("target/xtask/drift-report.json"));
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&summary)?;
    fs::write(&output_path, json)
        .with_context(|| format!("failed to write {}", output_path.display()))?;

    println!(
        "Drift summary written to {} (transitive: {}, advisories: {}, pin mismatches: {})",
        output_path.display(),
        summary.transitive_count,
        summary.advisories.count,
        summary.pin_mismatches.len()
    );

    if summary.transitive_count > TRANSITIVE_LIMIT {
        println!(
            "warning: transitive dependency count {} exceeds limit {}",
            summary.transitive_count, TRANSITIVE_LIMIT
        );
    }
    if summary.advisories.found {
        println!(
            "warning: {} advisories detected (see cargo audit output)",
            summary.advisories.count
        );
    }
    if !summary.pin_mismatches.is_empty() {
        println!("warning: version pin mismatches detected");
    }

    Ok(())
}

fn run(cmd: &str, args: impl IntoIterator<Item = &'static str>) -> Result<()> {
    let mut command = Command::new(cmd);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    let status = command
        .status()
        .with_context(|| format!("failed to spawn {}", cmd))?;
    if !status.success() {
        bail!("command `{cmd}` exited with {}", status);
    }
    Ok(())
}

fn run_capture(cmd: &str, args: impl IntoIterator<Item = &'static str>) -> Result<Vec<u8>> {
    let mut command = Command::new(cmd);
    command.args(args);
    command.stdin(Stdio::null());
    let output = command
        .output()
        .with_context(|| format!("failed to execute {}", cmd))?;
    if !output.status.success() {
        bail!("command `{cmd}` exited with {}", output.status);
    }
    Ok(output.stdout)
}

fn capture_tree_report() -> Result<()> {
    let stdout = run_capture(
        "cargo",
        [
            "tree",
            "--workspace",
            "--edges",
            "normal,build,dev",
            "--locked",
        ],
    )?;
    let out_dir = Path::new("target").join("xtask");
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;
    let path = out_dir.join("cargo-tree.txt");
    fs::write(&path, stdout).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn validate_feature_allowlist() -> Result<()> {
    let allowlist = load_feature_allowlist()?;
    if allowlist.is_empty() {
        return Ok(());
    }

    let metadata = load_metadata().context("failed to load cargo metadata")?;
    let resolve = metadata
        .resolve
        .ok_or_else(|| anyhow!("metadata.resolve missing"))?;

    let packages: HashMap<PackageId, _> = metadata
        .packages
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();

    let mut violations = Vec::new();
    for node in resolve.nodes {
        let Some(pkg) = packages.get(&node.id) else {
            continue;
        };

        if let Some(allowed_features) = allowlist.get(&pkg.name) {
            let unexpected: Vec<String> = node
                .features
                .iter()
                .filter_map(|feature| {
                    if feature == "default" || feature.starts_with("dep:") {
                        return None;
                    }
                    if allowed_features.contains(feature) {
                        None
                    } else {
                        Some(feature.clone())
                    }
                })
                .collect();

            if !unexpected.is_empty() {
                violations.push((pkg.name.clone(), unexpected));
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        let mut message = String::from("feature allowlist violations:\n");
        for (pkg, feats) in violations {
            message.push_str(&format!("  {pkg}: {:?}\n", feats));
        }
        bail!(message);
    }
}

fn load_feature_allowlist() -> Result<HashMap<String, BTreeSet<String>>> {
    let manifest = fs::read_to_string("Cargo.toml").context("failed to read Cargo.toml")?;
    let doc = manifest
        .parse::<toml::Value>()
        .context("failed to parse Cargo.toml")?;

    let allow = doc
        .get("workspace")
        .and_then(|w| w.get("metadata"))
        .and_then(|m| m.get("space"))
        .and_then(|s| s.get("allowed-features"))
        .and_then(|v| v.as_table());

    let mut map = HashMap::new();
    if let Some(table) = allow {
        for (pkg, value) in table {
            let list = value
                .as_array()
                .ok_or_else(|| anyhow!("allowed-features.{pkg} must be an array"))?;
            let mut set = BTreeSet::new();
            for item in list {
                let feature = item
                    .as_str()
                    .ok_or_else(|| anyhow!("allowed-features.{pkg} entries must be strings"))?;
                set.insert(feature.to_string());
            }
            map.insert(pkg.clone(), set);
        }
    }
    Ok(map)
}

fn load_version_pins() -> Result<BTreeMap<String, String>> {
    let manifest = fs::read_to_string("Cargo.toml").context("failed to read Cargo.toml")?;
    let doc = manifest
        .parse::<toml::Value>()
        .context("failed to parse Cargo.toml")?;
    let deps = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .ok_or_else(|| anyhow!("workspace.dependencies missing"))?;

    let mut map = BTreeMap::new();
    for (name, value) in deps {
        match value {
            toml::Value::String(v) => {
                map.insert(name.clone(), v.clone());
            }
            toml::Value::Table(table) => {
                if let Some(version) = table.get("version").and_then(|v| v.as_str()) {
                    map.insert(name.clone(), version.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(map)
}

fn find_pin_mismatches(metadata: &Metadata, pins: &BTreeMap<String, String>) -> Vec<PinMismatch> {
    let mut mismatches = Vec::new();
    let packages_by_name: HashMap<&str, Vec<&cargo_metadata::Package>> = metadata
        .packages
        .iter()
        .fold(HashMap::new(), |mut acc, pkg| {
            acc.entry(pkg.name.as_str())
                .or_insert_with(Vec::new)
                .push(pkg);
            acc
        });

    for (name, spec) in pins {
        let sanitized = spec.trim_start_matches(|c| c == '=' || c == '^');
        let resolved = packages_by_name
            .get(name.as_str())
            .map(|pkgs| {
                pkgs.iter()
                    .map(|p| p.version.to_string())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if resolved.is_empty() {
            continue;
        }

        let parsed_versions: Vec<Version> = resolved
            .iter()
            .filter_map(|s| Version::parse(s).ok())
            .collect();
        if parsed_versions.is_empty() {
            continue;
        }

        let req = match VersionReq::parse(spec)
            .or_else(|_| VersionReq::parse(&format!("={sanitized}")))
        {
            Ok(req) => req,
            Err(_) => {
                mismatches.push(PinMismatch {
                    name: name.clone(),
                    expected_spec: spec.clone(),
                    resolved,
                });
                continue;
            }
        };

        if !parsed_versions.iter().any(|v| req.matches(v)) {
            mismatches.push(PinMismatch {
                name: name.clone(),
                expected_spec: spec.clone(),
                resolved,
            });
        }
    }

    mismatches
}

fn count_transitive(metadata: &Metadata) -> usize {
    let workspace: HashSet<_> = metadata.workspace_members.iter().collect();
    metadata
        .resolve
        .as_ref()
        .map(|resolve| {
            resolve
                .nodes
                .iter()
                .filter(|node| !workspace.contains(&node.id))
                .map(|node| node.id.clone())
                .collect::<HashSet<_>>()
                .len()
        })
        .unwrap_or(0)
}

fn collect_audit_summary() -> Result<AdvisorySummary> {
    let output = Command::new("cargo")
        .args(["audit", "--json"])
        .output()
        .context("failed to run cargo audit --json")?;

    let value: Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse cargo audit json output")?;
    let vulnerabilities = value
        .get("vulnerabilities")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let found = vulnerabilities
        .get("found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let count = vulnerabilities
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let mut severities = BTreeMap::new();
    if let Some(list) = vulnerabilities.get("list").and_then(|v| v.as_array()) {
        for item in list {
            if let Some(advisory) = item.get("advisory") {
                let severity = advisory
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                *severities.entry(severity).or_insert(0usize) += 1;
            }
        }
    }

    Ok(AdvisorySummary {
        found,
        count,
        severities,
    })
}

fn load_metadata() -> Result<Metadata> {
    let mut cmd = MetadataCommand::new();
    cmd.other_options(vec!["--locked".to_string()]);
    cmd.exec().context("cargo metadata failed")
}

fn iso_timestamp() -> Result<String> {
    Ok(OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("failed to format timestamp")?)
}

#[derive(Serialize)]
struct DriftSummary {
    timestamp: String,
    advisories: AdvisorySummary,
    transitive_count: usize,
    transitive_limit: usize,
    pin_mismatches: Vec<PinMismatch>,
}

#[derive(Serialize)]
struct AdvisorySummary {
    found: bool,
    count: usize,
    severities: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct PinMismatch {
    name: String,
    expected_spec: String,
    resolved: Vec<String>,
}
