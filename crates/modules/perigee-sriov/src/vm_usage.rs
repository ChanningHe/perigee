//! Map PVE VM passthrough (`hostpci`) entries to the VFs they reference.

use perigee_core::ipc::VfUser;
use std::collections::HashMap;
use std::path::Path;

const PVE_QEMU_DIR: &str = "/etc/pve/qemu-server";

/// Scan every VM config for `hostpci` passthrough entries and map each
/// referenced PCI address (normalized, see [`normalize_pci`]) to the VM using
/// it and whether that VM is currently running. Unreadable configs (e.g. when
/// not run as root) simply yield an empty/partial map.
pub fn scan_vf_users() -> HashMap<String, VfUser> {
    let mut map = HashMap::new();
    let Ok(rd) = std::fs::read_dir(PVE_QEMU_DIR) else {
        return map;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("conf") {
            continue;
        }
        let Some(vmid) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let running = vm_is_running(vmid);
        for addr in hostpci_addrs(&content) {
            map.entry(normalize_pci(&addr)).or_insert_with(|| VfUser {
                vmid: vmid.to_string(),
                running,
            });
        }
    }
    map
}

/// A VM is running when its QEMU pid file exists.
fn vm_is_running(vmid: &str) -> bool {
    Path::new(&format!("/run/qemu-server/{}.pid", vmid)).exists()
}

/// Extract the raw PCI addresses from a config's `hostpci` lines. A value looks
/// like `0000:41:00.1,pcie=1` or `0000:41:00.1;0000:41:00.2`; mapping-based
/// entries (`mapping=foo`) are skipped since they carry no BDF.
fn hostpci_addrs(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with("hostpci") {
            continue;
        }
        let Some((_key, value)) = line.split_once(':') else {
            continue;
        };
        let devices = value.trim().split(',').next().unwrap_or("").trim();
        for addr in devices.split(';') {
            let addr = addr.trim();
            if addr.is_empty() || addr.contains('=') {
                continue;
            }
            out.push(addr.to_string());
        }
    }
    out
}

/// Normalize a PCI address for comparison: drop an optional `0000:` domain so
/// `0000:41:00.1` and `41:00.1` match, and lowercase the hex.
pub fn normalize_pci(addr: &str) -> String {
    addr.strip_prefix("0000:").unwrap_or(addr).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hostpci_lines() {
        let cfg = "\
name: test
hostpci0: 0000:41:00.1,pcie=1
hostpci1: 41:00.2
net0: virtio=AA:BB:CC:DD:EE:FF,bridge=vmbr0
hostpci2: mapping=mynic
";
        let addrs = hostpci_addrs(cfg);
        assert_eq!(addrs, vec!["0000:41:00.1", "41:00.2"]);
    }

    #[test]
    fn parses_multi_device_hostpci() {
        let cfg = "hostpci0: 0000:41:00.1;0000:41:00.2,pcie=1\n";
        assert_eq!(hostpci_addrs(cfg), vec!["0000:41:00.1", "0000:41:00.2"]);
    }

    #[test]
    fn normalize_matches_with_and_without_domain() {
        assert_eq!(normalize_pci("0000:41:00.1"), "41:00.1");
        assert_eq!(normalize_pci("41:00.1"), "41:00.1");
        assert_eq!(normalize_pci("0000:41:00.1"), normalize_pci("41:00.1"));
    }
}
