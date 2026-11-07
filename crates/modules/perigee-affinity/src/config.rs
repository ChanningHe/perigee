use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use serde::de::Error as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    SingleCcd,
    Balanced,
    Manual,
    PCoresOnly,
    ECoresOnly,
    AllCores,
}

impl std::str::FromStr for Strategy {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "single-ccd" => Ok(Self::SingleCcd),
            "balanced" => Ok(Self::Balanced),
            "manual" => Ok(Self::Manual),
            "p-cores-only" => Ok(Self::PCoresOnly),
            "e-cores-only" => Ok(Self::ECoresOnly),
            "all-cores" => Ok(Self::AllCores),
            _ => anyhow::bail!("unknown strategy: {value}"),
        }
    }
}

impl serde::Serialize for Strategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Self::SingleCcd => "single-ccd",
            Self::Balanced => "balanced",
            Self::Manual => "manual",
            Self::PCoresOnly => "p-cores-only",
            Self::ECoresOnly => "e-cores-only",
            Self::AllCores => "all-cores",
        })
    }
}

impl<'de> serde::Deserialize<'de> for Strategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}
use crate::model::{
    AffinityPolicy, AllocationPolicy, CorePreference, CpuSet, LocalityPolicy, OvercommitPolicy,
    SmtPolicy,
};

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

    /// Legacy strategy name retained as the compatibility input for the new
    /// policy model. New code should use `policy()` rather than matching it.
    #[serde(default = "default_strategy")]
    pub strategy: Strategy,

    /// Legacy switch: true selects sibling-aware placement; false selects
    /// logical-CPU-only placement.
    #[serde(default = "default_true")]
    pub include_smt: bool,

    /// Deprecated count-based reservation. Explicit `reserved_cpus` is the
    /// stable replacement and takes precedence when non-empty.
    #[serde(default)]
    pub reserve_cores: usize,

    #[serde(default)]
    pub reserved_cpus: Vec<usize>,

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
            reserve_cores: 0,
            reserved_cpus: Vec::new(),
            auto_apply: AutoApplyConfig::default(),
        }
    }
}

impl AffinityConfig {
    pub fn policy(&self) -> AffinityPolicy {
        let (allocation, core_preference) = match self.strategy {
            Strategy::SingleCcd => (AllocationPolicy::Packed, CorePreference::Any),
            Strategy::Balanced => (AllocationPolicy::Balanced, CorePreference::Any),
            Strategy::Manual => (AllocationPolicy::Balanced, CorePreference::Any),
            Strategy::PCoresOnly => (AllocationPolicy::Balanced, CorePreference::Performance),
            Strategy::ECoresOnly => (AllocationPolicy::Balanced, CorePreference::Efficiency),
            Strategy::AllCores => (AllocationPolicy::Spread, CorePreference::Any),
        };

        AffinityPolicy {
            allocation,
            core_preference,
            smt: if self.include_smt {
                SmtPolicy::PreferDistinctCores
            } else {
                SmtPolicy::LogicalOnly
            },
            locality: LocalityPolicy::PreferCacheDomain,
            overcommit: OvercommitPolicy::Exclusive,
            reserved_cpus: CpuSet::new(self.reserved_cpus.iter().copied()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_config_deserializes_without_string_round_trip() {
        let value: toml::Value = toml::from_str(
            r#"
            [affinity]
            enabled = true
            include_smt = true
            reserved_cpus = [0, 1]
            [affinity.auto_apply]
            enabled = true
            exclude_vmids = [102]
            "#,
        )
        .unwrap();
        let config: AffinityFileConfig = value.try_into().unwrap();
        assert!(config.affinity.auto_apply.enabled);
        assert_eq!(config.affinity.auto_apply.exclude_vmids, vec![102]);
        assert_eq!(config.affinity.policy().reserved_cpus.as_slice(), &[0, 1]);
    }

    #[test]
    fn invalid_strategy_is_an_explicit_error() {
        let error =
            toml::from_str::<AffinityFileConfig>("[affinity]\nstrategy = 'not-a-strategy'\n")
                .unwrap_err();
        assert!(error.to_string().contains("unknown strategy"));
    }
}
