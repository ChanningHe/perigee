//! Collect guest NICs (MAC + bridge) from the PVE guest configs.

use anyhow::{Context, Result};
use std::path::Path;

use crate::pve_conf;

const PVE_QEMU_DIR: &str = "/etc/pve/qemu-server";
const PVE_LXC_DIR: &str = "/etc/pve/lxc";

/// One guest NIC attached to a Linux bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestNic {
    pub vmid: String,
    pub mac: String,
    pub bridge: String,
}

/// Scan every local VM and CT config for bridged NICs.
///
/// Fails rather than returning a partial list: an empty or truncated result
/// would make the reconciler release entries that guests still need. In
/// particular the qemu-server dir must exist; its absence means pmxcfs is not
/// mounted yet (e.g. early boot), not that there are no guests.
pub fn scan_guest_nics() -> Result<Vec<GuestNic>> {
    let qemu = Path::new(PVE_QEMU_DIR);
    if !qemu.is_dir() {
        anyhow::bail!("{} not available (pmxcfs not mounted?)", PVE_QEMU_DIR);
    }
    let mut nics = scan_dir(qemu)?;
    let lxc = Path::new(PVE_LXC_DIR);
    if lxc.is_dir() {
        nics.extend(scan_dir(lxc)?);
    }
    Ok(nics)
}

fn scan_dir(dir: &Path) -> Result<Vec<GuestNic>> {
    let mut nics = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("conf") {
            continue;
        }
        let Some(vmid) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        nics.extend(
            parse_nics(&content)
                .into_iter()
                .map(|(mac, bridge)| GuestNic {
                    vmid: vmid.to_string(),
                    mac,
                    bridge,
                }),
        );
    }
    Ok(nics)
}

/// Extract `(mac, bridge)` pairs from the live `net<N>` lines of a guest
/// config. QEMU and LXC lines look like:
///   net0: virtio=52:54:00:a1:b2:c3,bridge=vmbr0,firewall=1
///   net0: name=eth0,bridge=vmbr0,hwaddr=BC:24:11:AA:BB:CC,type=veth
/// NICs without both a MAC and a bridge are skipped. MACs are lowercased.
pub fn parse_nics(content: &str) -> Vec<(String, String)> {
    pve_conf::main_section(content)
        .filter(|(key, _)| pve_conf::is_indexed_key(key, "net"))
        .filter_map(|(_, value)| parse_net_value(value))
        .collect()
}

fn parse_net_value(value: &str) -> Option<(String, String)> {
    let mut mac = None;
    let mut bridge = None;
    for part in value.split(',').map(str::trim) {
        if let Some(br) = part.strip_prefix("bridge=") {
            bridge = Some(br.trim().to_string());
        } else if let Some((_, v)) = part.split_once('=') {
            if is_mac_address(v) {
                mac = Some(v.to_lowercase());
            }
        }
    }
    Some((mac?, bridge?))
}

fn is_mac_address(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::parse_nics;

    fn pair(mac: &str, bridge: &str) -> (String, String) {
        (mac.to_string(), bridge.to_string())
    }

    #[test]
    fn collects_mac_and_bridge_per_nic() {
        let cfg = "\
name: testvm
net0: virtio=52:54:00:AA:BB:CC,bridge=vmbr0,firewall=1
net1: virtio=52:54:00:11:22:33,bridge=vmbr1
scsi0: local-lvm:vm-100-disk-0,size=32G
";
        assert_eq!(
            parse_nics(cfg),
            vec![
                pair("52:54:00:aa:bb:cc", "vmbr0"),
                pair("52:54:00:11:22:33", "vmbr1")
            ]
        );
    }

    #[test]
    fn ignores_snapshot_and_pending_sections() {
        // Only the live NIC counts; pending and snapshot MACs are not in use.
        let cfg = "\
net0: virtio=52:54:00:00:00:01,bridge=vmbr0
[PENDING]
net0: virtio=52:54:00:00:00:02,bridge=vmbr0
[pre-upgrade]
net0: virtio=52:54:00:00:00:03,bridge=vmbr0
";
        assert_eq!(parse_nics(cfg), vec![pair("52:54:00:00:00:01", "vmbr0")]);
    }

    #[test]
    fn only_netn_keys_count() {
        assert!(parse_nics("netx: virtio=52:54:00:00:00:09,bridge=vmbr0\n").is_empty());
    }

    #[test]
    fn parses_lxc_hwaddr() {
        let cfg = "net0: name=eth0,bridge=vmbr0,hwaddr=BC:24:11:AA:BB:CC,ip=dhcp,type=veth\n";
        assert_eq!(parse_nics(cfg), vec![pair("bc:24:11:aa:bb:cc", "vmbr0")]);
    }

    #[test]
    fn nic_without_bridge_or_mac_is_skipped() {
        assert!(parse_nics("net0: virtio=52:54:00:de:ad:be\n").is_empty());
        assert!(parse_nics("net0: virtio,bridge=vmbr0\n").is_empty());
    }
}
