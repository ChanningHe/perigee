use crate::error::{PerigeeError, Result};
use std::fs;
use std::path::{Path, PathBuf};

const SYS_CLASS_NET: &str = "/sys/class/net";

pub fn net_device_path(iface: &str) -> PathBuf {
    PathBuf::from(SYS_CLASS_NET).join(iface)
}

pub fn read_sysfs_value(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| PerigeeError::Sysfs(format!("failed to read {}: {}", path.display(), e)))
}

pub fn write_sysfs_value(path: &Path, value: &str) -> Result<()> {
    fs::write(path, value).map_err(|e| {
        PerigeeError::Sysfs(format!(
            "failed to write {} to {}: {}",
            value,
            path.display(),
            e
        ))
    })
}

pub fn list_net_interfaces() -> Result<Vec<String>> {
    let entries = fs::read_dir(SYS_CLASS_NET)
        .map_err(|e| PerigeeError::Sysfs(format!("failed to read {}: {}", SYS_CLASS_NET, e)))?;

    let mut ifaces = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PerigeeError::Sysfs(e.to_string()))?;
        if let Some(name) = entry.file_name().to_str() {
            ifaces.push(name.to_string());
        }
    }
    Ok(ifaces)
}

pub fn read_iface_address(iface: &str) -> Result<String> {
    let path = net_device_path(iface).join("address");
    read_sysfs_value(&path)
}

pub fn read_sriov_totalvfs(iface: &str) -> Result<u32> {
    let path = net_device_path(iface).join("device/sriov_totalvfs");
    read_sysfs_value(&path)?
        .parse()
        .map_err(|e| PerigeeError::Sysfs(format!("invalid sriov_totalvfs for {}: {}", iface, e)))
}

pub fn read_sriov_numvfs(iface: &str) -> Result<u32> {
    let path = net_device_path(iface).join("device/sriov_numvfs");
    read_sysfs_value(&path)?
        .parse()
        .map_err(|e| PerigeeError::Sysfs(format!("invalid sriov_numvfs for {}: {}", iface, e)))
}

pub fn write_sriov_numvfs(iface: &str, num: u32) -> Result<()> {
    let path = net_device_path(iface).join("device/sriov_numvfs");
    write_sysfs_value(&path, &num.to_string())
}

pub fn has_sriov_support(iface: &str) -> bool {
    net_device_path(iface)
        .join("device/sriov_totalvfs")
        .exists()
}

/// Returns true if this interface is a VF (has a `physfn` symlink).
pub fn is_virtual_function(iface: &str) -> bool {
    net_device_path(iface).join("device/physfn").exists()
}

pub fn read_device_vendor(iface: &str) -> Result<String> {
    let path = net_device_path(iface).join("device/vendor");
    read_sysfs_value(&path)
}

pub fn read_device_id(iface: &str) -> Result<String> {
    let path = net_device_path(iface).join("device/device");
    read_sysfs_value(&path)
}

/// PCI address (BDF, e.g. "0000:41:00.1") of VF `vf_index` on `iface`, read from
/// the `device/virtfn<N>` symlink. This is the address PVE uses to pass a VF
/// through to a guest. None if it cannot be resolved.
pub fn read_vf_pci_addr(iface: &str, vf_index: u32) -> Option<String> {
    let link = net_device_path(iface).join(format!("device/virtfn{}", vf_index));
    let target = fs::read_link(&link).ok()?;
    target.file_name().map(|n| n.to_string_lossy().to_string())
}

pub fn read_driver_name(iface: &str) -> Result<String> {
    let link = net_device_path(iface).join("device/driver");
    let target = fs::read_link(&link).map_err(|e| {
        PerigeeError::Sysfs(format!("failed to read driver link for {}: {}", iface, e))
    })?;
    target
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| PerigeeError::Sysfs(format!("invalid driver path for {}", iface)))
}

pub fn read_pci_address(iface: &str) -> Result<String> {
    let path = net_device_path(iface).join("device/uevent");
    let content = read_sysfs_value(&path)?;
    for line in content.lines() {
        if let Some(addr) = line.strip_prefix("PCI_SLOT_NAME=") {
            return Ok(addr.to_string());
        }
    }
    Err(PerigeeError::Sysfs(format!(
        "PCI_SLOT_NAME not found in uevent for {}",
        iface
    )))
}

pub fn read_link_operstate(iface: &str) -> Result<String> {
    let path = net_device_path(iface).join("operstate");
    read_sysfs_value(&path)
}

pub fn read_link_speed(iface: &str) -> Result<String> {
    let path = net_device_path(iface).join("speed");
    read_sysfs_value(&path)
}

/// Find the physical interface that has the given MAC address.
///
/// Virtual devices stacked on a NIC (bridges, bonds, VLANs) usually inherit
/// its MAC, e.g. PVE's vmbr0 takes the first port's address, so a bare address
/// match could return `vmbr0` instead of the PF depending on sysfs order. Only
/// interfaces backed by a device (`/sys/class/net/<if>/device`) are matched.
pub fn find_iface_by_mac(mac: &str) -> Result<String> {
    let candidates = list_net_interfaces()?.into_iter().filter_map(|iface| {
        let addr = read_iface_address(&iface).ok()?;
        let is_physical = net_device_path(&iface).join("device").exists();
        Some((iface, addr, is_physical))
    });
    first_physical_match(mac, candidates)
        .ok_or_else(|| PerigeeError::InterfaceNotFound(mac.to_string()))
}

/// First `(iface, address, is_physical)` candidate that is physical and has
/// `mac` (case-insensitive).
fn first_physical_match(
    mac: &str,
    candidates: impl IntoIterator<Item = (String, String, bool)>,
) -> Option<String> {
    candidates
        .into_iter()
        .find(|(_, addr, is_physical)| *is_physical && addr.eq_ignore_ascii_case(mac))
        .map(|(iface, _, _)| iface)
}

#[cfg(test)]
mod tests {
    use super::first_physical_match;

    fn c(iface: &str, addr: &str, physical: bool) -> (String, String, bool) {
        (iface.to_string(), addr.to_string(), physical)
    }

    #[test]
    fn skips_virtual_devices_sharing_the_pf_mac() {
        let candidates = [
            c("vmbr0", "b8:ce:f6:00:00:01", false),
            c("bond0", "b8:ce:f6:00:00:01", false),
            c("enp1s0f0", "B8:CE:F6:00:00:01", true),
        ];
        assert_eq!(
            first_physical_match("b8:ce:f6:00:00:01", candidates).as_deref(),
            Some("enp1s0f0")
        );
    }

    #[test]
    fn no_physical_match_is_none() {
        let candidates = [c("vmbr0", "b8:ce:f6:00:00:01", false)];
        assert!(first_physical_match("b8:ce:f6:00:00:01", candidates).is_none());
    }
}
