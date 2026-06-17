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

/// Find the interface name that matches a given MAC address.
pub fn find_iface_by_mac(mac: &str) -> Result<String> {
    let mac_lower = mac.to_lowercase();
    for iface in list_net_interfaces()? {
        if let Ok(addr) = read_iface_address(&iface) {
            if addr.to_lowercase() == mac_lower {
                return Ok(iface);
            }
        }
    }
    Err(PerigeeError::Sysfs(format!(
        "no interface found with MAC {}",
        mac
    )))
}
