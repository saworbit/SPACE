//! Lightweight NFS-style protocol view backed by the common capsule registry.
//!
//! The object-core (capsules + segments) is protocol agnostic.  This module offers
//! a very small "file + directory" façade that understands POSIX-like paths and
//! maps them onto capsules via the shared [`WritePipeline`].  The design goals are:
//!   * keep the in-memory namespace data-structure simple and deterministic so that
//!     tests can reason about ordering,
//!   * avoid leaking capsules on overwrite/delete by running the pipeline GC
//!     helpers where appropriate, and
//!   * provide rich doc comments / inline rationale so that future protocol teams
//!     understand the trade-offs made here.
//!     The implementation is intentionally conservative: it serialises namespace
//!     mutations through an `RwLock` and rewrites whole files on every modification.

use anyhow::{anyhow, bail, Result};
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::CapsuleId;
use nvram_sim::NvramLog;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Public metadata returned to callers.  We expose only the minimum that higher
/// layers (CLI/tests) need today; additional fields can be wired through later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NfsEntry {
    path: String,
    name: String,
    #[serde(rename = "type")]
    kind: EntryKind,
    size: u64,
    created_at: u64,
    modified_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    capsule_id: Option<CapsuleId>,
}

impl NfsEntry {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_directory(&self) -> bool {
        matches!(self.kind, EntryKind::Directory)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn modified_at(&self) -> u64 {
        self.modified_at
    }

    pub fn capsule_id(&self) -> Option<CapsuleId> {
        self.capsule_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum EntryKind {
    File,
    Directory,
}

/// Internal representation tracking each path in the namespace.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NfsNode {
    name: String,
    path: String,
    kind: NfsNodeKind,
    created_at: u64,
    modified_at: u64,
}

impl NfsNode {
    fn directory(path: &str, name: &str, timestamp: u64) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            kind: NfsNodeKind::Directory,
            created_at: timestamp,
            modified_at: timestamp,
        }
    }

    fn file(path: &str, name: &str, capsule_id: CapsuleId, size: u64, timestamp: u64) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            kind: NfsNodeKind::File { capsule_id, size },
            created_at: timestamp,
            modified_at: timestamp,
        }
    }

    fn to_entry(&self) -> NfsEntry {
        match self.kind {
            NfsNodeKind::Directory => NfsEntry {
                path: self.path.clone(),
                name: self.name.clone(),
                kind: EntryKind::Directory,
                size: 0,
                created_at: self.created_at,
                modified_at: self.modified_at,
                capsule_id: None,
            },
            NfsNodeKind::File { capsule_id, size } => NfsEntry {
                path: self.path.clone(),
                name: self.name.clone(),
                kind: EntryKind::File,
                size,
                created_at: self.created_at,
                modified_at: self.modified_at,
                capsule_id: Some(capsule_id),
            },
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self.kind, NfsNodeKind::Directory)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum NfsNodeKind {
    Directory,
    File { capsule_id: CapsuleId, size: u64 },
}

/// Normalised POSIX path helper used to enforce canonical keys inside the map.
#[derive(Debug, Clone)]
struct NormalizedPath {
    full: String,
    components: Vec<String>,
}

impl NormalizedPath {
    fn new(full: String, components: Vec<String>) -> Self {
        Self { full, components }
    }

    fn full(&self) -> &str {
        &self.full
    }

    fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    fn name(&self) -> Option<&str> {
        self.components.last().map(|s| s.as_str())
    }

    fn parent_path(&self) -> Option<String> {
        if self.components.is_empty() {
            None
        } else if self.components.len() == 1 {
            Some("/".to_string())
        } else {
            Some(format!(
                "/{}",
                self.components[..self.components.len() - 1].join("/")
            ))
        }
    }
}

/// Simple NFS namespace façade backed by capsules.
pub struct NfsView {
    pipeline: Arc<WritePipeline>,
    nodes: Arc<RwLock<BTreeMap<String, NfsNode>>>,
    namespace_path: Option<PathBuf>,
}

impl NfsView {
    /// Create a new NFS view.  We eagerly normalise the root entry so that all
    /// operations can assume it exists.
    pub fn new(registry: CapsuleRegistry, nvram: NvramLog) -> Self {
        let pipeline = Arc::new(WritePipeline::new(registry, nvram));
        let mut nodes = BTreeMap::new();
        let now = unix_timestamp();
        ensure_root_node(&mut nodes, now);

        Self {
            pipeline,
            nodes: Arc::new(RwLock::new(nodes)),
            namespace_path: None,
        }
    }

    /// Open a view backed by an on-disk namespace description (JSON).
    pub fn open<P: AsRef<Path>>(
        registry: CapsuleRegistry,
        nvram: NvramLog,
        namespace_path: P,
    ) -> Result<Self> {
        let pipeline = Arc::new(WritePipeline::new(registry, nvram));
        let path = namespace_path.as_ref();
        let mut nodes = if path.exists() {
            let data = fs::read_to_string(path)?;
            serde_json::from_str(&data)?
        } else {
            BTreeMap::new()
        };

        let now = unix_timestamp();
        ensure_root_node(&mut nodes, now);

        Ok(Self {
            pipeline,
            nodes: Arc::new(RwLock::new(nodes)),
            namespace_path: Some(path.to_path_buf()),
        })
    }

    fn persist(&self) -> Result<()> {
        if let Some(path) = &self.namespace_path {
            let nodes = self.nodes.read().unwrap();
            let json = serde_json::to_string_pretty(&*nodes)?;
            fs::write(path, json)?;
        }
        Ok(())
    }

    /// Write (create or overwrite) a file at `path`.
    ///
    /// Rationale: we allocate a brand new capsule per write to keep the metadata
    /// immutable.  If a previous file existed, we ask the pipeline to delete the
    /// superseded capsule once the new data is durable.
    pub fn write_file(&self, path: &str, data: Vec<u8>) -> Result<CapsuleId> {
        let path_info = normalize_path(path)?;
        if path_info.is_root() {
            bail!("Cannot write file at root");
        }

        let parent_path = path_info
            .parent_path()
            .ok_or_else(|| anyhow!("File path must have a parent directory"))?;

        let parent_info = normalize_path(&parent_path)?;

        let now = unix_timestamp();

        // Pre-flight check for directory collisions while avoiding holding the write lock.
        {
            let nodes = self.nodes.read().unwrap();
            if let Some(existing) = nodes.get(path_info.full()) {
                if existing.is_directory() {
                    bail!("Cannot overwrite directory with file");
                }
            }
        }

        let capsule_id = self.pipeline.write_capsule(&data)?;
        let mut nodes = self.nodes.write().unwrap();
        ensure_directory(&mut nodes, &parent_info, now)?;

        // Capture old capsule (if any) so that we can drop it after updating metadata.
        let old_capsule = nodes
            .get(path_info.full())
            .and_then(|node| match node.kind {
                NfsNodeKind::File { capsule_id, .. } => Some(capsule_id),
                NfsNodeKind::Directory => None,
            });

        let file_name = path_info
            .name()
            .ok_or_else(|| anyhow!("Invalid file path"))?;

        nodes.insert(
            path_info.full().to_string(),
            NfsNode::file(
                path_info.full(),
                file_name,
                capsule_id,
                data.len() as u64,
                now,
            ),
        );

        // Touch parent directory modified timestamp to reflect the change.
        if let Some(parent_node) = nodes.get_mut(parent_info.full()) {
            parent_node.modified_at = now;
        }

        drop(nodes);

        if let Some(old_capsule) = old_capsule {
            // Ignore errors when deleting the old capsule – GC will eventually clean up.
            let _ = self.pipeline.delete_capsule(old_capsule);
        }

        self.persist()?;

        Ok(capsule_id)
    }

    /// Read the full file contents for `path`.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let path_info = normalize_path(path)?;
        let node = {
            let nodes = self.nodes.read().unwrap();
            nodes
                .get(path_info.full())
                .cloned()
                .ok_or_else(|| anyhow!("No such file: {}", path_info.full()))?
        };

        match node.kind {
            NfsNodeKind::File { capsule_id, .. } => self.pipeline.read_capsule(capsule_id),
            NfsNodeKind::Directory => bail!("Path is a directory: {}", node.path),
        }
    }

    /// Read a byte range from the file at `path`.
    pub fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let path_info = normalize_path(path)?;
        let node = {
            let nodes = self.nodes.read().unwrap();
            nodes
                .get(path_info.full())
                .cloned()
                .ok_or_else(|| anyhow!("No such file: {}", path_info.full()))?
        };

        match node.kind {
            NfsNodeKind::File { capsule_id, size } => {
                if offset + len as u64 > size {
                    bail!("Read beyond end of file");
                }
                self.pipeline.read_range(capsule_id, offset, len)
            }
            NfsNodeKind::Directory => bail!("Path is a directory: {}", node.path),
        }
    }

    /// Explicitly create a directory and its parents.
    pub fn mkdir(&self, path: &str) -> Result<()> {
        let path_info = normalize_path(path)?;
        let now = unix_timestamp();
        let mut nodes = self.nodes.write().unwrap();
        ensure_directory(&mut nodes, &path_info, now)?;
        drop(nodes);
        self.persist()
    }

    /// Delete a file or empty directory.  Directories must be empty to avoid
    /// accidentally orphaning entries.
    pub fn delete(&self, path: &str) -> Result<()> {
        let path_info = normalize_path(path)?;
        if path_info.is_root() {
            bail!("Cannot delete root directory");
        }

        let now = unix_timestamp();
        let mut nodes = self.nodes.write().unwrap();

        let node = nodes
            .get(path_info.full())
            .cloned()
            .ok_or_else(|| anyhow!("No such path: {}", path_info.full()))?;

        let mut removed_capsule = None;

        match node.kind {
            NfsNodeKind::Directory => {
                let prefix = format!("{}/", path_info.full());
                if nodes.keys().any(|k| k.starts_with(&prefix)) {
                    bail!("Directory not empty: {}", path_info.full());
                }
                nodes.remove(path_info.full());
            }
            NfsNodeKind::File { capsule_id, .. } => {
                nodes.remove(path_info.full());
                removed_capsule = Some(capsule_id);
            }
        }

        if let Some(parent_path) = path_info.parent_path() {
            if let Some(parent_node) = nodes.get_mut(&parent_path) {
                parent_node.modified_at = now;
            }
        }

        drop(nodes);

        if let Some(capsule_id) = removed_capsule {
            let _ = self.pipeline.delete_capsule(capsule_id);
        }

        self.persist()?;

        Ok(())
    }

    /// Return metadata for the path.
    pub fn metadata(&self, path: &str) -> Result<NfsEntry> {
        let path_info = normalize_path(path)?;
        let nodes = self.nodes.read().unwrap();
        nodes
            .get(path_info.full())
            .map(|node| node.to_entry())
            .ok_or_else(|| anyhow!("No such path: {}", path_info.full()))
    }

    /// List the immediate children of `path` (files + directories).
    pub fn list_directory(&self, path: &str) -> Result<Vec<NfsEntry>> {
        let path_info = normalize_path(path)?;
        let nodes = self.nodes.read().unwrap();
        let dir_node = nodes
            .get(path_info.full())
            .ok_or_else(|| anyhow!("No such directory: {}", path_info.full()))?;
        if !dir_node.is_directory() {
            bail!("Path is not a directory: {}", path_info.full());
        }

        let prefix = if path_info.is_root() {
            "/".to_string()
        } else {
            format!("{}/", path_info.full())
        };

        let mut entries = Vec::new();
        for (path, node) in nodes.iter() {
            if path == path_info.full() {
                continue;
            }

            if !path.starts_with(&prefix) {
                continue;
            }

            let remainder = &path[prefix.len()..];
            if remainder.is_empty() || remainder.contains('/') {
                continue;
            }

            entries.push(node.to_entry());
        }

        Ok(entries)
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalize_path(path: &str) -> Result<NormalizedPath> {
    let candidate = if path.is_empty() { "/" } else { path };
    let path = Path::new(candidate);
    let mut components = Vec::new();
    let mut absolute = false;

    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                absolute = true;
                components.clear();
            }
            Component::CurDir => { /* ignore */ }
            Component::ParentDir => {
                components.pop();
            }
            Component::Normal(part) => {
                let part = part.to_string_lossy().replace('\\', "/");
                if part.is_empty() {
                    continue;
                }
                components.push(part);
            }
        }
    }

    if !absolute {
        // Treat relative paths as rooted at "/" to keep semantics simple.
        absolute = true;
    }

    if !absolute && components.is_empty() {
        bail!("Invalid path");
    }

    let full = if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    };

    Ok(NormalizedPath::new(full, components))
}

fn ensure_directory(
    nodes: &mut BTreeMap<String, NfsNode>,
    path: &NormalizedPath,
    timestamp: u64,
) -> Result<()> {
    ensure_root_node(nodes, timestamp);

    let mut current_components: Vec<String> = Vec::new();

    for part in &path.components {
        current_components.push(part.clone());
        let current_path = format!("/{}", current_components.join("/"));

        match nodes.get_mut(&current_path) {
            Some(node) if node.is_directory() => {
                // Update modified timestamp when we walk through existing directories.
                node.modified_at = timestamp;
            }
            Some(_) => bail!("Path conflict with file: {}", current_path),
            None => {
                let name = part.clone();
                nodes.insert(
                    current_path.clone(),
                    NfsNode::directory(&current_path, &name, timestamp),
                );
            }
        }
    }

    Ok(())
}

fn ensure_root_node(nodes: &mut BTreeMap<String, NfsNode>, timestamp: u64) {
    nodes
        .entry("/".to_string())
        .and_modify(|node| {
            node.modified_at = timestamp;
        })
        .or_insert_with(|| NfsNode::directory("/", "/", timestamp));
}
