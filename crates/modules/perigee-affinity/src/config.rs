use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::affinity::Strategy;

const CONFIG_FILENAME: &str = "affinity.toml";

pub fn affinity_config_path() -> PathBuf {
    PathBuf::from("/etc/perigee").join(CONFIG_FILENAME)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AffinityFileConfig {
    #[serde(default)]
    pub affinity: AffinityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffinityConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_strategy")]
    pub strategy: Strategy,

    #[serde(default = "default_true")]
    pub include_smt: bool,

    #[serde(default = "default_reserve")]
    pub reserve_cores: usize,

    #[serde(default)]
    pub auto_apply: AutoApplyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutoApplyConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub exclude_vmids: Vec<u32>,
}

impl Default for AffinityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: Strategy::Balanced,
            include_smt: true,
            reserve_cores: 2,
            auto_apply: AutoApplyConfig::default(),
        }
    }
}

impl AffinityFileConfig {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_strategy() -> Strategy {
    Strategy::Balanced
}

fn default_reserve() -> usize {
    2
}
