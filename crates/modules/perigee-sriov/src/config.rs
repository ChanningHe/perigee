use anyhow::{bail, Context, Result};
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
    /// Opt-in PF lossless-Ethernet setup for RoCE. Absent (the default) means
    /// perigee never touches the PF's DCB/pause/RoCE settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roce: Option<RoceConfig>,
}

fn default_mac_strategy() -> MacStrategyConfig {
    MacStrategyConfig::Sequential
}

impl SriovProfileConfig {
    /// Validate hardware-independent constraints. The VF count is checked
    /// against the PF's sriov_totalvfs at apply time, where the device is known.
    pub fn validate(&self) -> Result<()> {
        if let Some(vlan) = &self.defaults.vlan {
            vlan.validate().context("defaults")?;
        }
        for vf in &self.vf {
            if let Some(vlan) = &vf.vlan {
                vlan.validate()
                    .with_context(|| format!("VF {}", vf.index))?;
            }
        }
        // `roce` is deliberately not checked here: this runs before VFs are
        // created, and a bad RoCE section must never block them. The RoCE
        // step validates it and reports a failure as a degraded apply.
        Ok(())
    }

    /// The RoCE section, only when present and switched on.
    pub fn enabled_roce(&self) -> Option<&RoceConfig> {
        self.roce.as_ref().filter(|r| r.enabled)
    }

    pub fn roce_enabled(&self) -> bool {
        self.enabled_roce().is_some()
    }
}

/// PF settings for RoCE over lossless Ethernet. The DSCP/priority values must
/// match the switch side; the defaults follow the common NVIDIA/Mellanox RoCE
/// profile (data DSCP 26 on priority 3, CNP DSCP 48 on priority 6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoceConfig {
    #[serde(default)]
    pub enabled: bool,
    /// DSCP of RoCE data traffic, mapped to `priority`.
    #[serde(default = "default_roce_dscp")]
    pub dscp: u8,
    /// Priority that carries RoCE data; the only priority with PFC enabled.
    #[serde(default = "default_roce_priority")]
    pub priority: u8,
    /// DSCP of congestion notification packets, mapped to `cnp_priority`.
    #[serde(default = "default_cnp_dscp")]
    pub cnp_dscp: u8,
    #[serde(default = "default_cnp_priority")]
    pub cnp_priority: u8,
    /// mlx5 `pfc-prevention-tout` tunable: 0 disables, else 80-8000 ms.
    #[serde(default = "default_pfc_storm_timeout_ms")]
    pub pfc_storm_timeout_ms: u16,
}

const MAX_DSCP: u8 = 63;
const MAX_PRIORITY: u8 = 7;
/// mlx5 accepts 0 (disabled) or MLX5E_PFC_PREVEN_TOUT_{MIN,MAX}_MSEC.
const PFC_STORM_TIMEOUT_MIN_MS: u16 = 80;
const PFC_STORM_TIMEOUT_MAX_MS: u16 = 8000;
/// ECN codepoint ECT(0) in the low two bits of the ToS byte.
const ECN_ECT0: u8 = 0b10;

fn default_roce_dscp() -> u8 {
    26
}
fn default_roce_priority() -> u8 {
    3
}
fn default_cnp_dscp() -> u8 {
    48
}
fn default_cnp_priority() -> u8 {
    6
}
fn default_pfc_storm_timeout_ms() -> u16 {
    100
}

impl Default for RoceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dscp: default_roce_dscp(),
            priority: default_roce_priority(),
            cnp_dscp: default_cnp_dscp(),
            cnp_priority: default_cnp_priority(),
            pfc_storm_timeout_ms: default_pfc_storm_timeout_ms(),
        }
    }
}

impl RoceConfig {
    pub fn validate(&self) -> Result<()> {
        for (name, dscp) in [("dscp", self.dscp), ("cnp_dscp", self.cnp_dscp)] {
            if dscp > MAX_DSCP {
                bail!("{} {} out of range (0-{})", name, dscp, MAX_DSCP);
            }
        }
        for (name, prio) in [
            ("priority", self.priority),
            ("cnp_priority", self.cnp_priority),
        ] {
            if prio > MAX_PRIORITY {
                bail!("{} {} out of range (0-{})", name, prio, MAX_PRIORITY);
            }
        }
        if self.dscp == self.cnp_dscp {
            bail!("dscp and cnp_dscp must differ (both {})", self.dscp);
        }
        let tout = self.pfc_storm_timeout_ms;
        if tout != 0 && !(PFC_STORM_TIMEOUT_MIN_MS..=PFC_STORM_TIMEOUT_MAX_MS).contains(&tout) {
            bail!(
                "pfc_storm_timeout_ms {} out of range (0 or {}-{})",
                tout,
                PFC_STORM_TIMEOUT_MIN_MS,
                PFC_STORM_TIMEOUT_MAX_MS
            );
        }
        Ok(())
    }

    /// RDMA CM default ToS: DSCP in the upper six bits, ECT(0) in the ECN bits
    /// (DSCP 26 -> 106). `None` for a DSCP that does not fit in six bits.
    pub fn roce_tos(&self) -> Option<u8> {
        (self.dscp <= MAX_DSCP).then_some((self.dscp << 2) | ECN_ECT0)
    }
}

/// Administrative VF link state, as `ip link set <pf> vf <n> state <value>`.
/// `auto` follows the PF uplink; `enable` keeps the VF up when the uplink drops.
/// Only enforced when configured; an absent key leaves the VF untouched.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum VfLinkState {
    #[default]
    Auto,
    Enable,
    Disable,
}

impl VfLinkState {
    const ALL: [VfLinkState; 3] = [Self::Auto, Self::Enable, Self::Disable];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

/// TUI cycling order: unmanaged (`None`) -> auto -> enable -> disable -> unmanaged.
pub fn next_link_state(current: Option<VfLinkState>) -> Option<VfLinkState> {
    match current {
        None => Some(VfLinkState::Auto),
        Some(VfLinkState::Auto) => Some(VfLinkState::Enable),
        Some(VfLinkState::Enable) => Some(VfLinkState::Disable),
        Some(VfLinkState::Disable) => None,
    }
}

impl std::fmt::Display for VfLinkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for VfLinkState {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or_else(|| format!("unknown VF link state '{}'", s))
    }
}

impl VlanConfig {
    /// 802.1Q VLAN ids are 1-4094 (0 clears the tag, 4095 is reserved) and the
    /// 802.1p priority (qos) is 0-7. The TUI enforces these, but a hand-edited
    /// sriov.toml is unchecked until here.
    pub fn validate(&self) -> Result<()> {
        if !(1..=4094).contains(&self.id) {
            bail!("VLAN id {} out of range (1-4094)", self.id);
        }
        if let Some(qos) = self.qos {
            if qos > 7 {
                bail!("VLAN qos {} out of range (0-7)", qos);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MacStrategyConfig {
    #[default]
    Sequential,
    Random,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfDefaults {
    #[serde(default = "default_true")]
    pub trust: bool,
    #[serde(default)]
    pub spoofchk: bool,
    #[serde(default)]
    pub vlan: Option<VlanConfig>,
    /// `None` (key absent): perigee leaves VF link state alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_state: Option<VfLinkState>,
}

impl Default for VfDefaults {
    fn default() -> Self {
        Self {
            trust: true,
            spoofchk: false,
            vlan: None,
            link_state: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_state: Option<VfLinkState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlanConfig {
    pub id: u16,
    #[serde(default)]
    pub qos: Option<u8>,
    #[serde(default)]
    pub proto: Option<VlanProto>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum VlanProto {
    #[serde(rename = "802.1Q")]
    #[default]
    Dot1Q,
    #[serde(rename = "802.1ad")]
    Dot1Ad,
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
#[derive(Default)]
pub enum FdbMode {
    #[serde(rename = "daemon_watch")]
    #[default]
    DaemonWatch,
    Hookscript,
    Disabled,
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
        assert!(profile.validate().is_ok());
    }

    fn vlan(id: u16, qos: Option<u8>) -> VlanConfig {
        VlanConfig {
            id,
            qos,
            proto: None,
        }
    }

    #[test]
    fn valid_vlan_passes() {
        assert!(vlan(100, Some(7)).validate().is_ok());
        assert!(vlan(1, None).validate().is_ok());
        assert!(vlan(4094, Some(0)).validate().is_ok());
    }

    #[test]
    fn out_of_range_vlan_rejected() {
        assert!(vlan(0, None).validate().is_err());
        assert!(vlan(4095, None).validate().is_err());
        assert!(vlan(100, Some(8)).validate().is_err());
    }

    /// The production sriov.toml as written by perigee <= 0.1.4: no
    /// link_state, no roce section.
    const LEGACY_TOML: &str = r#"
[sriov.enp65s0np0]
mac = "8c:2a:8e:88:9a:f6"
num_vfs = 64
mac_strategy = "sequential"

[sriov.enp65s0np0.defaults]
trust = true
spoofchk = false

[[sriov.enp65s0np0.vf]]
index = 20
trust = false

[sriov.enp65s0np0.vf.vlan]
id = 200

[sriov.enp65s0np0.fdb]
mode = "daemon_watch"
"#;

    fn profile_from(toml_str: &str) -> SriovProfileConfig {
        let config: SriovFileConfig = toml::from_str(toml_str).unwrap();
        config.sriov.into_values().next().unwrap()
    }

    #[test]
    fn legacy_config_defaults_to_auto_link_state_and_no_roce() {
        let profile = profile_from(LEGACY_TOML);
        assert_eq!(profile.defaults.link_state, None, "unmanaged");
        assert_eq!(profile.vf[0].link_state, None);
        assert!(profile.roce.is_none());
        assert!(!profile.roce_enabled());
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn link_state_parses_in_defaults_and_overrides() {
        let profile = profile_from(
            r#"
[sriov.p]
mac = "8c:2a:8e:88:9a:f6"
num_vfs = 4
[sriov.p.defaults]
link_state = "enable"
[[sriov.p.vf]]
index = 1
link_state = "disable"
[[sriov.p.vf]]
index = 2
link_state = "auto"
"#,
        );
        assert_eq!(profile.defaults.link_state, Some(VfLinkState::Enable));
        assert_eq!(profile.vf[0].link_state, Some(VfLinkState::Disable));
        assert_eq!(profile.vf[1].link_state, Some(VfLinkState::Auto));
    }

    #[test]
    fn link_state_cycle_includes_unmanaged() {
        use VfLinkState::*;
        assert_eq!(next_link_state(None), Some(Auto));
        assert_eq!(next_link_state(Some(Auto)), Some(Enable));
        assert_eq!(next_link_state(Some(Enable)), Some(Disable));
        assert_eq!(next_link_state(Some(Disable)), None);
    }

    #[test]
    fn unmanaged_link_state_is_not_written_back() {
        let profile = profile_from(LEGACY_TOML);
        let mut map = BTreeMap::new();
        map.insert("p".to_string(), profile);
        let text = toml::to_string_pretty(&SriovFileConfig { sriov: map }).unwrap();
        assert!(!text.contains("link_state"), "{text}");
    }

    #[test]
    fn unknown_link_state_is_a_parse_error() {
        let res: Result<SriovFileConfig, _> = toml::from_str(
            r#"
[sriov.p]
mac = "8c:2a:8e:88:9a:f6"
num_vfs = 4
[sriov.p.defaults]
link_state = "up"
"#,
        );
        assert!(res.is_err());
    }

    #[test]
    fn link_state_str_matches_ip_link_keywords() {
        assert_eq!(VfLinkState::Auto.as_str(), "auto");
        assert_eq!(VfLinkState::Enable.as_str(), "enable");
        assert_eq!(VfLinkState::Disable.as_str(), "disable");
        assert_eq!("enable".parse::<VfLinkState>(), Ok(VfLinkState::Enable));
        assert!("up".parse::<VfLinkState>().is_err());
    }

    #[test]
    fn roce_section_with_only_enabled_gets_switch_side_defaults() {
        let profile = profile_from(
            r#"
[sriov.p]
mac = "8c:2a:8e:88:9a:f6"
num_vfs = 4
[sriov.p.roce]
enabled = true
"#,
        );
        let roce = profile.roce.as_ref().unwrap();
        assert!(roce.enabled);
        assert_eq!(roce.dscp, 26);
        assert_eq!(roce.priority, 3);
        assert_eq!(roce.cnp_dscp, 48);
        assert_eq!(roce.cnp_priority, 6);
        assert_eq!(roce.pfc_storm_timeout_ms, 100);
        assert!(profile.roce_enabled());
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn roce_section_without_enabled_is_off() {
        let profile = profile_from(
            r#"
[sriov.p]
mac = "8c:2a:8e:88:9a:f6"
num_vfs = 4
[sriov.p.roce]
dscp = 24
"#,
        );
        assert!(!profile.roce.as_ref().unwrap().enabled);
        assert!(!profile.roce_enabled());
    }

    fn roce(f: impl FnOnce(&mut RoceConfig)) -> RoceConfig {
        let mut cfg = RoceConfig {
            enabled: true,
            ..RoceConfig::default()
        };
        f(&mut cfg);
        cfg
    }

    #[test]
    fn roce_tos_is_dscp_shifted_with_ect0() {
        assert_eq!(roce(|_| {}).roce_tos(), Some(106));
        assert_eq!(roce(|c| c.dscp = 24).roce_tos(), Some(98));
        assert_eq!(roce(|c| c.dscp = 63).roce_tos(), Some(254));
    }

    #[test]
    fn roce_tos_refuses_out_of_range_dscp() {
        // 64 << 2 would silently wrap to 0 in a u8.
        assert_eq!(roce(|c| c.dscp = 64).roce_tos(), None);
    }

    #[test]
    fn roce_validation_accepts_edges() {
        assert!(roce(|c| c.dscp = 0).validate().is_ok());
        assert!(roce(|c| c.dscp = 63).validate().is_ok());
        assert!(roce(|c| c.priority = 7).validate().is_ok());
        assert!(roce(|c| c.cnp_priority = 0).validate().is_ok());
        assert!(roce(|c| c.pfc_storm_timeout_ms = 0).validate().is_ok());
        assert!(roce(|c| c.pfc_storm_timeout_ms = 80).validate().is_ok());
        assert!(roce(|c| c.pfc_storm_timeout_ms = 8000).validate().is_ok());
    }

    #[test]
    fn roce_validation_rejects_out_of_range() {
        assert!(roce(|c| c.dscp = 64).validate().is_err());
        assert!(roce(|c| c.cnp_dscp = 64).validate().is_err());
        assert!(roce(|c| c.priority = 8).validate().is_err());
        assert!(roce(|c| c.cnp_priority = 8).validate().is_err());
        assert!(roce(|c| c.pfc_storm_timeout_ms = 79).validate().is_err());
        assert!(roce(|c| c.pfc_storm_timeout_ms = 8001).validate().is_err());
    }

    #[test]
    fn roce_validation_rejects_shared_dscp() {
        assert!(roce(|c| c.cnp_dscp = c.dscp).validate().is_err());
    }

    #[test]
    fn profile_validate_ignores_roce_so_vfs_are_never_blocked() {
        // An invalid roce section is reported by the RoCE step itself; it
        // must not abort the apply before sriov_numvfs is written.
        let mut profile = profile_from(LEGACY_TOML);
        for enabled in [false, true] {
            profile.roce = Some(RoceConfig {
                enabled,
                cnp_dscp: 26,
                pfc_storm_timeout_ms: 50,
                ..RoceConfig::default()
            });
            assert!(profile.validate().is_ok());
        }
    }

    #[test]
    fn enabled_roce_gate() {
        let mut profile = profile_from(LEGACY_TOML);
        assert!(profile.enabled_roce().is_none());
        profile.roce = Some(RoceConfig::default());
        assert!(profile.enabled_roce().is_none());
        profile.roce = Some(roce(|_| {}));
        assert_eq!(profile.enabled_roce(), Some(&roce(|_| {})));
    }

    #[test]
    fn new_fields_survive_serialize_round_trip() {
        let mut profile = profile_from(LEGACY_TOML);
        profile.defaults.link_state = Some(VfLinkState::Enable);
        profile.vf[0].link_state = Some(VfLinkState::Disable);
        profile.roce = Some(roce(|c| c.priority = 4));
        let mut map = BTreeMap::new();
        map.insert("p".to_string(), profile);
        let text = toml::to_string_pretty(&SriovFileConfig { sriov: map }).unwrap();

        let back = profile_from(&text);
        assert_eq!(back.defaults.link_state, Some(VfLinkState::Enable));
        assert_eq!(back.vf[0].link_state, Some(VfLinkState::Disable));
        assert_eq!(back.roce, Some(roce(|c| c.priority = 4)));
    }

    #[test]
    fn absent_roce_stays_absent_after_round_trip() {
        let profile = profile_from(LEGACY_TOML);
        let mut map = BTreeMap::new();
        map.insert("p".to_string(), profile);
        let text = toml::to_string_pretty(&SriovFileConfig { sriov: map }).unwrap();
        assert!(!text.contains("roce"));
        assert!(profile_from(&text).roce.is_none());
    }
}
