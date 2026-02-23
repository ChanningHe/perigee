use perigee_core::mac::MacAddress;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CONFIG_DIR: &str = "/etc/perigee";

pub fn sriov_config_path() -> PathBuf {
    PathBuf::from(CONFIG_DIR).join("sriov.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SriovFileConfig {
    pub sriov: BTreeMap<String, SriovProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SriovProfileConfig {
    pub mac: MacAddress,
    pub num_vfs: u32,
    #[serde(default = "default_mac_strategy")]
    pub mac_strategy: MacStrategyConfig,
    #[serde(default)]
    pub defaults: VfDefaults,
    #[serde(default)]
    pub vf: Vec<VfOverride>,
    #[serde(default)]
    pub fdb: FdbConfig,
}

fn default_mac_strategy() -> MacStrategyConfig {
    MacStrategyConfig::Sequential
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MacStrategyConfig {
    Sequential,
    Random,
    Custom,
}

impl Default for MacStrategyConfig {
    fn default() -> Self {
        Self::Sequential
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfDefaults {
    #[serde(default = "default_true")]
    pub trust: bool,
    #[serde(default)]
    pub spoofchk: bool,
    #[serde(default)]
    pub vlan: Option<VlanConfig>,
}

impl Default for VfDefaults {
    fn default() -> Self {
        Self {
            trust: true,
            spoofchk: false,
            vlan: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfOverride {
    pub index: u32,
    #[serde(default)]
    pub mac: Option<MacAddress>,
    #[serde(default)]
    pub trust: Option<bool>,
    #[serde(default)]
    pub spoofchk: Option<bool>,
    #[serde(default)]
    pub vlan: Option<VlanConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlanConfig {
    pub id: u16,
    #[serde(default)]
    pub qos: Option<u8>,
    #[serde(default)]
    pub proto: Option<VlanProto>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VlanProto {
    #[serde(rename = "802.1Q")]
    Dot1Q,
    #[serde(rename = "802.1ad")]
    Dot1Ad,
}

impl Default for VlanProto {
    fn default() -> Self {
        Self::Dot1Q
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FdbConfig {
    #[serde(default = "default_fdb_mode")]
    pub mode: FdbMode,
    #[serde(default)]
    pub output_path: Option<String>,
}

impl Default for FdbConfig {
    fn default() -> Self {
        Self {
            mode: FdbMode::DaemonWatch,
            output_path: None,
        }
    }
}

fn default_fdb_mode() -> FdbMode {
    FdbMode::DaemonWatch
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FdbMode {
    #[serde(rename = "daemon_watch")]
    DaemonWatch,
    Hookscript,
    Disabled,
}

impl Default for FdbMode {
    fn default() -> Self {
        Self::DaemonWatch
    }
}

impl SriovFileConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_toml() {
        let toml_str = r#"
[sriov.lab-cx6-port0]
mac = "b8:ce:f6:12:34:56"
num_vfs = 16
mac_strategy = "sequential"

[sriov.lab-cx6-port0.defaults]
trust = true
spoofchk = false

[[sriov.lab-cx6-port0.vf]]
index = 0
vlan = { id = 100, qos = 0 }

[[sriov.lab-cx6-port0.vf]]
index = 3
vlan = { id = 200, qos = 2 }
trust = false

[sriov.lab-cx6-port0.fdb]
mode = "hookscript"
output_path = "/var/lib/vz/snippets/perigee-bridgefix.sh"
"#;

        let config: SriovFileConfig = toml::from_str(toml_str).unwrap();
        let profile = &config.sriov["lab-cx6-port0"];
        assert_eq!(profile.num_vfs, 16);
        assert_eq!(profile.mac_strategy, MacStrategyConfig::Sequential);
        assert!(profile.defaults.trust);
        assert!(!profile.defaults.spoofchk);
        assert_eq!(profile.vf.len(), 2);
        assert_eq!(profile.vf[0].index, 0);
        assert_eq!(profile.vf[0].vlan.as_ref().unwrap().id, 100);
        assert_eq!(profile.fdb.mode, FdbMode::Hookscript);
    }
}
