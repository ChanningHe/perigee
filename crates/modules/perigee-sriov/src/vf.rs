use anyhow::{bail, Context, Result};
use perigee_core::mac::MacAddress;
use perigee_core::sysfs;
use tracing::{info, warn};

use crate::config::{SriovProfileConfig, VfLinkState, VfOverride, VlanConfig};
use crate::mac_strategy::MacStrategy;

/// Apply a complete SR-IOV profile: create VFs and configure each one.
pub fn apply_profile(profile_name: &str, config: &SriovProfileConfig) -> Result<ApplyResult> {
    let pf_mac = config.mac.to_string();
    let pf_iface = sysfs::find_iface_by_mac(&pf_mac)
        .context(format!("cannot locate PF with MAC {}", pf_mac))?;

    info!(profile = %profile_name, pf = %pf_iface, mac = %pf_mac, "applying SR-IOV profile");

    // Reject a bad config before touching any sysfs state.
    config.validate()?;
    if let Ok(total) = sysfs::read_sriov_totalvfs(&pf_iface) {
        if config.num_vfs > total {
            bail!(
                "profile requests {} VFs but {} supports at most {}",
                config.num_vfs,
                pf_iface,
                total
            );
        }
    }

    let mut result = ApplyResult {
        pf_iface: pf_iface.clone(),
        total_vfs: config.num_vfs,
        created_vfs: 0,
        configured_vfs: 0,
        errors: Vec::new(),
        warnings: Vec::new(),
    };

    // Step 1: Ensure PF is up
    if let Err(e) = set_link_up(&pf_iface) {
        warn!(pf = %pf_iface, error = %e, "failed to bring PF up");
    }

    let numvfs = sysfs::read_sriov_numvfs(&pf_iface).ok();
    let current = numvfs.unwrap_or(0);

    // Step 2: Opt-in PF RoCE/lossless QoS, before any VF is (re)created. The
    // PFC change toggles the PF link once, so it is only made while no VFs
    // exist (boot); with VFs present (or an unreadable count) it is deferred.
    // Only what differs is written. Failures never block VF creation.
    if let Some(roce) = config.enabled_roce() {
        let outcome = crate::roce::apply_pf_qos(&pf_iface, roce, numvfs);
        result.errors.extend(outcome.errors);
        result.warnings.extend(outcome.warnings);
    }

    // Step 3: Create/reset VFs only if count differs
    if current == config.num_vfs {
        info!(pf = %pf_iface, vfs = current, "VF count already matches, skipping reset");
    } else {
        if current > 0 {
            info!(pf = %pf_iface, current_vfs = current, target_vfs = config.num_vfs, "resetting VFs");
            if let Err(e) = sysfs::write_sriov_numvfs(&pf_iface, 0) {
                result.errors.push(format!("failed to reset VFs: {}", e));
                return Ok(result);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        if let Err(e) = sysfs::write_sriov_numvfs(&pf_iface, config.num_vfs) {
            result
                .errors
                .push(format!("failed to create {} VFs: {}", config.num_vfs, e));
            return Ok(result);
        }
    }

    let actual = sysfs::read_sriov_numvfs(&pf_iface).unwrap_or(0);
    result.created_vfs = actual;
    if actual != config.num_vfs {
        result.errors.push(format!(
            "requested {} VFs but only {} created",
            config.num_vfs, actual
        ));
    }

    // Step 4: Generate MACs
    let strategy = MacStrategy::from_config(&config.mac_strategy, &config.mac);
    let macs = strategy.generate(actual);

    // Current link states, so `state` is only written where it differs. A
    // failed read leaves every VF "unknown" (see needs_link_state_write).
    let live_states = read_actual_vf_states(&pf_iface).unwrap_or_else(|e| {
        warn!(pf = %pf_iface, error = %e, "cannot read VF link states");
        Vec::new()
    });

    // Step 5: Configure each VF
    for vf_index in 0..actual {
        let vf_mac = get_vf_mac(vf_index, &macs, &config.vf);
        let flags = vf_flags(vf_index, config);
        let vlan = get_vf_vlan(vf_index, config);
        let live_link_state = live_states
            .iter()
            .find(|s| s.index == vf_index)
            .and_then(|s| s.link_state);

        match configure_single_vf(
            &pf_iface,
            vf_index,
            &vf_mac,
            flags,
            vlan.as_ref(),
            live_link_state,
        ) {
            Ok(()) => result.configured_vfs += 1,
            Err(e) => result
                .errors
                .push(format!("VF#{} config failed: {}", vf_index, e)),
        }
    }

    info!(
        profile = %profile_name,
        created = result.created_vfs,
        configured = result.configured_vfs,
        errors = result.errors.len(),
        "profile apply complete"
    );

    Ok(result)
}

#[derive(Debug)]
pub struct ApplyResult {
    pub pf_iface: String,
    pub total_vfs: u32,
    pub created_vfs: u32,
    pub configured_vfs: u32,
    pub errors: Vec<String>,
    /// Notable but non-failing conditions (e.g. a PF link flap on a live host).
    pub warnings: Vec<String>,
}

impl ApplyResult {
    pub fn is_success(&self) -> bool {
        self.errors.is_empty() && self.created_vfs == self.total_vfs
    }

    pub fn is_degraded(&self) -> bool {
        !self.errors.is_empty() && self.configured_vfs > 0
    }
}

fn get_vf_mac(index: u32, generated: &[MacAddress], overrides: &[VfOverride]) -> MacAddress {
    if let Some(ov) = overrides.iter().find(|o| o.index == index) {
        if let Some(mac) = &ov.mac {
            return *mac;
        }
    }
    generated
        .get(index as usize)
        .copied()
        .unwrap_or(MacAddress::ZERO)
}

/// Effective per-VF flags: profile defaults with the VF override applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfFlags {
    pub trust: bool,
    pub spoofchk: bool,
    /// `None`: link state is not managed for this VF.
    pub link_state: Option<VfLinkState>,
}

pub fn vf_flags(index: u32, config: &SriovProfileConfig) -> VfFlags {
    let ov = config.vf.iter().find(|o| o.index == index);
    VfFlags {
        trust: ov.and_then(|o| o.trust).unwrap_or(config.defaults.trust),
        spoofchk: ov
            .and_then(|o| o.spoofchk)
            .unwrap_or(config.defaults.spoofchk),
        link_state: ov.and_then(|o| o.link_state).or(config.defaults.link_state),
    }
}

/// Whether `ip link set <pf> vf <n> state` must be issued: only for a managed
/// (explicitly configured) state that differs from, or cannot be compared
/// with, the live one. Unmanaged VFs are never touched.
fn needs_link_state_write(desired: Option<VfLinkState>, actual: Option<VfLinkState>) -> bool {
    desired.is_some_and(|d| actual != Some(d))
}

fn get_vf_vlan(index: u32, config: &SriovProfileConfig) -> Option<VlanConfig> {
    if let Some(ov) = config.vf.iter().find(|o| o.index == index) {
        if ov.vlan.is_some() {
            return ov.vlan.clone();
        }
    }
    config.defaults.vlan.clone()
}

/// `ip link` argument lists that configure one VF, in order. VLAN is always
/// set (0 clears a stale tag); link state comes last so a failure there can
/// never leave a freshly created VF without its VLAN.
fn vf_ip_link_commands(
    pf: &str,
    index: u32,
    mac: &MacAddress,
    flags: VfFlags,
    vlan: Option<&VlanConfig>,
    actual_link_state: Option<VfLinkState>,
) -> Vec<Vec<String>> {
    let vf = |verb: &str, value: String| {
        vec![
            "set".to_string(),
            pf.to_string(),
            "vf".to_string(),
            index.to_string(),
            verb.to_string(),
            value,
        ]
    };
    let on_off = |b: bool| if b { "on" } else { "off" }.to_string();

    let mut vlan_cmd = vf("vlan", vlan.map(|v| v.id).unwrap_or(0).to_string());
    if let Some(qos) = vlan.and_then(|v| v.qos) {
        vlan_cmd.push("qos".to_string());
        vlan_cmd.push(qos.to_string());
    }

    let mut cmds = vec![
        vf("mac", mac.to_string()),
        vf("trust", on_off(flags.trust)),
        vf("spoofchk", on_off(flags.spoofchk)),
        vlan_cmd,
    ];
    if let Some(state) = flags
        .link_state
        .filter(|_| needs_link_state_write(flags.link_state, actual_link_state))
    {
        cmds.push(vf("state", state.as_str().to_string()));
    }
    cmds
}

fn configure_single_vf(
    pf: &str,
    index: u32,
    mac: &MacAddress,
    flags: VfFlags,
    vlan: Option<&VlanConfig>,
    actual_link_state: Option<VfLinkState>,
) -> Result<()> {
    for args in vf_ip_link_commands(pf, index, mac, flags, vlan, actual_link_state) {
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_ip_link(&args_ref)?;
    }

    let vlan_str = vlan
        .map(|v| {
            if let Some(qos) = v.qos {
                format!("{} qos={}", v.id, qos)
            } else {
                v.id.to_string()
            }
        })
        .unwrap_or_else(|| "-".into());
    let link_str = flags.link_state.map_or("unmanaged", |s| s.as_str());
    info!(
        pf = %pf,
        vf = index,
        mac = %mac,
        trust = flags.trust,
        spoofchk = flags.spoofchk,
        link_state = link_str,
        vlan = %vlan_str,
        "VF configured"
    );
    Ok(())
}

/// Actual state of a single VF as read from `ip link show`.
#[derive(Debug, Clone)]
pub struct VfActualState {
    pub index: u32,
    pub mac: String,
    pub trust: bool,
    pub spoofchk: bool,
    pub vlan_id: Option<u16>,
    /// `None` when the driver does not report a link state.
    pub link_state: Option<VfLinkState>,
}

/// Read actual VF states from `ip -d link show <pf>`.
pub fn read_actual_vf_states(pf_iface: &str) -> Result<Vec<VfActualState>> {
    let output = std::process::Command::new("ip")
        .args(["-d", "link", "show", pf_iface])
        .output()
        .context("failed to execute `ip link show`")?;

    if !output.status.success() {
        bail!(
            "ip link show {} failed: {}",
            pf_iface,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_vf_lines(&stdout))
}

fn parse_vf_lines(output: &str) -> Vec<VfActualState> {
    let mut results = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("vf ") {
            continue;
        }
        // "vf 3     link/ether 98:03:9b:95:b6:fc brd ..., vlan 120, spoof checking off, ... trust on, ..."
        let parts: Vec<&str> = trimmed.splitn(2, "link/ether ").collect();
        if parts.len() < 2 {
            continue;
        }

        let index: u32 = trimmed
            .strip_prefix("vf ")
            .and_then(|s| s.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(u32::MAX);
        if index == u32::MAX {
            continue;
        }

        let rest = parts[1];
        let mac = rest.split_whitespace().next().unwrap_or("").to_string();

        let trust = rest.contains("trust on");
        let spoofchk = rest.contains("spoof checking on");

        let vlan_id = if let Some(pos) = rest.find(", vlan ") {
            let after = &rest[pos + 7..];
            after
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse::<u16>().ok())
                .filter(|&v| v > 0)
        } else {
            None
        };

        let link_state = rest
            .split(", ")
            .find_map(|field| field.strip_prefix("link-state "))
            .and_then(|v| v.trim().parse().ok());

        results.push(VfActualState {
            index,
            mac,
            trust,
            spoofchk,
            vlan_id,
            link_state,
        });
    }
    results
}

fn set_link_up(iface: &str) -> Result<()> {
    run_ip_link(&["set", iface, "up"])
}

fn run_ip_link(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("ip")
        .arg("link")
        .args(args)
        .output()
        .context("failed to execute `ip link`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ip link {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SriovFileConfig, VfLinkState};

    /// Captured from `ip -d link show enp65s0np0` on serverhub (PVE 9.2,
    /// iproute2-6.15.0, ConnectX-5 mlx5_core, legacy eswitch).
    const IP_D_LINK_SERVERHUB: &str = "\
4: enp65s0np0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc mq master vmbr0 state UP mode DEFAULT group default qlen 1000
    link/ether 8c:2a:8e:88:9a:f6 brd ff:ff:ff:ff:ff:ff promiscuity 3 allmulti 3 minmtu 68 maxmtu 9978
    vf 0     link/ether 8c:2a:8e:88:9a:f7 brd ff:ff:ff:ff:ff:ff, spoof checking off, link-state auto, trust on, query_rss off
    vf 7     link/ether 8c:2a:8e:88:9a:fe brd ff:ff:ff:ff:ff:ff, vlan 4020, spoof checking off, link-state auto, trust on, query_rss off
    vf 20     link/ether 8c:2a:8e:88:9b:0b brd ff:ff:ff:ff:ff:ff, vlan 200, spoof checking off, link-state auto, trust off, query_rss off
";

    #[test]
    fn parses_real_serverhub_vf_lines() {
        let vfs = parse_vf_lines(IP_D_LINK_SERVERHUB);
        assert_eq!(vfs.len(), 3);
        assert_eq!(vfs[0].index, 0);
        assert_eq!(vfs[0].mac, "8c:2a:8e:88:9a:f7");
        assert!(vfs[0].trust);
        assert!(!vfs[0].spoofchk);
        assert_eq!(vfs[0].vlan_id, None);
        assert_eq!(vfs[0].link_state, Some(VfLinkState::Auto));
        assert_eq!(vfs[1].vlan_id, Some(4020));
        assert_eq!(vfs[2].index, 20);
        assert!(!vfs[2].trust);
        assert_eq!(vfs[2].vlan_id, Some(200));
    }

    #[test]
    fn parses_enable_and_disable_link_state() {
        let out = "    vf 1     link/ether 8c:2a:8e:88:9a:f8 brd ff:ff:ff:ff:ff:ff, spoof checking off, link-state enable, trust on, query_rss off\n    vf 2     link/ether 8c:2a:8e:88:9a:f9 brd ff:ff:ff:ff:ff:ff, spoof checking on, link-state disable, trust off, query_rss off\n";
        let vfs = parse_vf_lines(out);
        assert_eq!(vfs[0].link_state, Some(VfLinkState::Enable));
        assert_eq!(vfs[1].link_state, Some(VfLinkState::Disable));
        assert!(vfs[1].spoofchk);
    }

    #[test]
    fn missing_link_state_reads_as_unknown() {
        // Drivers without IFLA_VF_LINK_STATE print no link-state field.
        let out = "    vf 0     link/ether 8c:2a:8e:88:9a:f7 brd ff:ff:ff:ff:ff:ff, spoof checking off, trust on\n";
        assert_eq!(parse_vf_lines(out)[0].link_state, None);
    }

    #[test]
    fn link_state_written_only_when_managed_and_different() {
        use VfLinkState::*;
        // Unmanaged (no link_state anywhere): never written, whatever the VF has.
        assert!(!needs_link_state_write(None, Some(Enable)));
        assert!(!needs_link_state_write(None, None));
        // Managed: written on mismatch only.
        assert!(!needs_link_state_write(Some(Auto), Some(Auto)));
        assert!(!needs_link_state_write(Some(Enable), Some(Enable)));
        assert!(needs_link_state_write(Some(Enable), Some(Auto)));
        assert!(needs_link_state_write(Some(Auto), Some(Enable)));
        // Managed but unreadable: an explicit request is issued.
        assert!(needs_link_state_write(Some(Auto), None));
    }

    fn mac() -> MacAddress {
        "8c:2a:8e:88:9a:fe".parse().unwrap()
    }

    fn flags(link_state: Option<VfLinkState>) -> VfFlags {
        VfFlags {
            trust: true,
            spoofchk: false,
            link_state,
        }
    }

    #[test]
    fn vf_commands_set_state_last_after_vlan() {
        let vlan = VlanConfig {
            id: 4020,
            qos: None,
            proto: None,
        };
        let cmds = vf_ip_link_commands(
            "enp65s0np0",
            7,
            &mac(),
            flags(Some(VfLinkState::Enable)),
            Some(&vlan),
            Some(VfLinkState::Auto),
        );
        let verbs: Vec<&str> = cmds.iter().map(|c| c[4].as_str()).collect();
        assert_eq!(verbs, ["mac", "trust", "spoofchk", "vlan", "state"]);
        assert_eq!(
            cmds.last().unwrap(),
            &["set", "enp65s0np0", "vf", "7", "state", "enable"].map(String::from)
        );
        assert_eq!(
            cmds[3],
            ["set", "enp65s0np0", "vf", "7", "vlan", "4020"].map(String::from)
        );
    }

    #[test]
    fn vf_commands_omit_state_when_unmanaged_or_matching() {
        for (desired, live) in [
            (None, Some(VfLinkState::Enable)),
            (Some(VfLinkState::Enable), Some(VfLinkState::Enable)),
        ] {
            let cmds = vf_ip_link_commands("p", 0, &mac(), flags(desired), None, live);
            assert!(cmds.iter().all(|c| c[4] != "state"));
            // VLAN is always (re)set, 0 clears a stale tag.
            assert_eq!(
                cmds.last().unwrap(),
                &["set", "p", "vf", "0", "vlan", "0"].map(String::from)
            );
        }
    }

    #[test]
    fn vf_flags_apply_override_over_defaults() {
        let config: SriovFileConfig = toml::from_str(
            r#"
[sriov.p]
mac = "8c:2a:8e:88:9a:f6"
num_vfs = 4
[sriov.p.defaults]
link_state = "enable"
[[sriov.p.vf]]
index = 2
trust = false
link_state = "auto"
"#,
        )
        .unwrap();
        let p = &config.sriov["p"];
        let f0 = vf_flags(0, p);
        assert!(f0.trust);
        assert_eq!(f0.link_state, Some(VfLinkState::Enable));
        let f2 = vf_flags(2, p);
        assert!(!f2.trust);
        assert!(!f2.spoofchk);
        assert_eq!(f2.link_state, Some(VfLinkState::Auto));
    }

    #[test]
    fn vf_flags_without_link_state_are_unmanaged() {
        let config: SriovFileConfig =
            toml::from_str("[sriov.p]\nmac = \"8c:2a:8e:88:9a:f6\"\nnum_vfs = 2\n").unwrap();
        assert_eq!(vf_flags(0, &config.sriov["p"]).link_state, None);
    }
}
