use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::ZoneConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ZoneDirectory {
    pub zones: Vec<ZoneConfig>,
}

impl ZoneDirectory {
    pub fn resolve(name_or_alias: &str) -> Option<String> {
        let trimmed = name_or_alias.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn get(&self, name_or_alias: &str) -> Option<&ZoneConfig> {
        let key = Self::resolve(name_or_alias)?;
        self.zones
            .iter()
            .find(|z| z.name.eq_ignore_ascii_case(&key))
    }

    pub fn upsert(&mut self, zone: ZoneConfig) {
        if let Some(existing) = self
            .zones
            .iter_mut()
            .find(|z| z.name.eq_ignore_ascii_case(&zone.name))
        {
            *existing = zone;
            return;
        }
        self.zones.push(zone);
        self.zones.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
    }

    pub fn remove(&mut self, name_or_alias: &str) -> bool {
        let Some(key) = Self::resolve(name_or_alias) else {
            return false;
        };
        let before = self.zones.len();
        self.zones.retain(|z| !z.name.eq_ignore_ascii_case(&key));
        before != self.zones.len()
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read zones config {}", path.display()))?;
        let parsed = serde_json::from_str(&text)
            .with_context(|| format!("parse zones config {}", path.display()))?;
        Ok(parsed)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("serialize zones config")?;
        std::fs::write(path, format!("{text}\n"))
            .with_context(|| format!("write zones config {}", path.display()))?;
        Ok(())
    }

    pub fn default_path() -> Result<PathBuf> {
        if let Ok(path) = std::env::var("SPACE_ZONES_PATH") {
            return Ok(PathBuf::from(path));
        }

        let home = std::env::var_os("SPACE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .context("resolve SPACE_HOME/HOME/USERPROFILE for zone config")?;

        Ok(home.join(".space").join("zones.json"))
    }
}
