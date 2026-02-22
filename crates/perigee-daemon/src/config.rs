use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

const CONFIG_DIR: &str = "/etc/perigee";

pub fn config_dir() -> PathBuf {
    PathBuf::from(CONFIG_DIR)
}

pub fn sriov_config_path() -> PathBuf {
    config_dir().join("sriov.toml")
}

/// Load all TOML config files from /etc/perigee/ and merge into a single toml::Value.
pub fn load_all_configs() -> Result<toml::Value> {
    let dir = config_dir();
    if !dir.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }

    let mut merged = toml::map::Map::new();

    let entries = std::fs::read_dir(&dir)
        .with_context(|| format!("failed to read config dir {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match load_single_config(&path) {
            Ok(table) => {
                for (key, value) in table {
                    merged.insert(key, value);
                }
                info!(path = %path.display(), "config loaded");
            }
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "failed to load config");
            }
        }
    }

    Ok(toml::Value::Table(merged))
}

fn load_single_config(path: &Path) -> Result<toml::map::Map<String, toml::Value>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    match value {
        toml::Value::Table(t) => Ok(t),
        _ => anyhow::bail!("{} is not a TOML table", path.display()),
    }
}
