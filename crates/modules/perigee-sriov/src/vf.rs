use anyhow::{bail, Context, Result};
use perigee_core::mac::MacAddress;
use perigee_core::sysfs;
use tracing::{info, warn};

use crate::config::{SriovProfileConfig, VfOverride, VlanConfig};
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
    };

    // Step 1: Ensure PF is up
    if let Err(e) = set_link_up(&pf_iface) {
        warn!(pf = %pf_iface, error = %e, "failed to bring PF up");
    }

    // Step 2: Create/reset VFs only if count differs
    let current = sysfs::read_sriov_numvfs(&pf_iface).unwrap_or(0);
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

    // Step 5: Configure each VF
    for vf_index in 0..actual {
        let vf_mac = get_vf_mac(vf_index, &macs, &config.vf);
        let (trust, spoofchk) = get_vf_flags(vf_index, config);
        let vlan = get_vf_vlan(vf_index, config);

        match configure_single_vf(&pf_iface, vf_index, &vf_mac, trust, spoofchk, vlan.as_ref()) {
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

fn get_vf_flags(index: u32, config: &SriovProfileConfig) -> (bool, bool) {
    let mut trust = config.defaults.trust;
    let mut spoofchk = config.defaults.spoofchk;
    if let Some(ov) = config.vf.iter().find(|o| o.index == index) {
        if let Some(t) = ov.trust {
            trust = t;
        }
        if let Some(s) = ov.spoofchk {
            spoofchk = s;
        }
    }
    (trust, spoofchk)
}

fn get_vf_vlan(index: u32, config: &SriovProfileConfig) -> Option<VlanConfig> {
    if let Some(ov) = config.vf.iter().find(|o| o.index == index) {
        if ov.vlan.is_some() {
            return ov.vlan.clone();
        }
    }
    config.defaults.vlan.clone()
}

fn configure_single_vf(
    pf: &str,
    index: u32,
    mac: &MacAddress,
    trust: bool,
    spoofchk: bool,
    vlan: Option<&VlanConfig>,
) -> Result<()> {
    // Set MAC
    run_ip_link(&["set", pf, "vf", &index.to_string(), "mac", &mac.to_string()])?;

    // Set trust
    let trust_val = if trust { "on" } else { "off" };
    run_ip_link(&["set", pf, "vf", &index.to_string(), "trust", trust_val])?;

    // Set spoofchk
    let spoof_val = if spoofchk { "on" } else { "off" };
    run_ip_link(&["set", pf, "vf", &index.to_string(), "spoofchk", spoof_val])?;

    // Always set VLAN: explicit id or 0 to clear stale VLAN
    {
        let vlan_id = vlan.map(|v| v.id).unwrap_or(0);
        let mut args = vec![
            "set".to_string(),
            pf.to_string(),
            "vf".to_string(),
            index.to_string(),
            "vlan".to_string(),
            vlan_id.to_string(),
        ];
        if let Some(qos) = vlan.and_then(|v| v.qos) {
            args.push("qos".to_string());
            args.push(qos.to_string());
        }
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
    info!(pf = %pf, vf = index, mac = %mac, trust, spoofchk, vlan = %vlan_str, "VF configured");
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

        results.push(VfActualState {
            index,
            mac,
            trust,
            spoofchk,
            vlan_id,
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
