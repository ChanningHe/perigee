use anyhow::Result;
use perigee_core::mac::MacAddress;
use perigee_core::sysfs;

use crate::vendor::NicVendor;

#[derive(Debug, Clone)]
pub struct PhysicalFunction {
    pub iface_name: String,
    pub pci_address: String,
    pub mac_address: MacAddress,
    pub vendor: NicVendor,
    pub max_vfs: u32,
    pub current_vfs: u32,
    pub driver: String,
    pub link_state: LinkState,
    pub speed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Up,
    Down,
    Unknown,
}

impl std::fmt::Display for LinkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Up => write!(f, "UP"),
            Self::Down => write!(f, "DOWN"),
            Self::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

/// Scan sysfs for all SR-IOV capable physical functions.
pub fn scan_physical_functions() -> Result<Vec<PhysicalFunction>> {
    let interfaces = sysfs::list_net_interfaces()?;
    let mut pfs = Vec::new();

    for iface in &interfaces {
        if !sysfs::has_sriov_support(iface) {
            continue;
        }
        // Skip virtual functions (they have a physfn symlink)
        if sysfs::is_virtual_function(iface) {
            continue;
        }

        match build_pf_info(iface) {
            Ok(pf) => pfs.push(pf),
            Err(e) => {
                tracing::warn!(iface = %iface, error = %e, "skipping interface");
            }
        }
    }

    pfs.sort_by(|a, b| a.pci_address.cmp(&b.pci_address));
    Ok(pfs)
}

/// Find a PF by its MAC address (stable identifier).
pub fn find_pf_by_mac(mac: &MacAddress) -> Result<PhysicalFunction> {
    let iface = sysfs::find_iface_by_mac(&mac.to_string())?;
    build_pf_info(&iface)
}

fn build_pf_info(iface: &str) -> Result<PhysicalFunction> {
    let mac_str = sysfs::read_iface_address(iface)?;
    let mac_address: MacAddress = mac_str.parse().map_err(|e| anyhow::anyhow!("{}", e))?;
    let pci_address = sysfs::read_pci_address(iface)?;
    let max_vfs = sysfs::read_sriov_totalvfs(iface)?;
    let current_vfs = sysfs::read_sriov_numvfs(iface)?;
    let driver = sysfs::read_driver_name(iface).unwrap_or_else(|_| "unknown".to_string());
    let vendor = detect_vendor(iface);

    let operstate = sysfs::read_link_operstate(iface).unwrap_or_default();
    let link_state = match operstate.as_str() {
        "up" => LinkState::Up,
        "down" => LinkState::Down,
        _ => LinkState::Unknown,
    };
    let speed = sysfs::read_link_speed(iface).ok();

    Ok(PhysicalFunction {
        iface_name: iface.to_string(),
        pci_address,
        mac_address,
        vendor,
        max_vfs,
        current_vfs,
        driver,
        link_state,
        speed,
    })
}

fn detect_vendor(iface: &str) -> NicVendor {
    let vendor_id = sysfs::read_device_vendor(iface).unwrap_or_default();
    NicVendor::from_vendor_id(&vendor_id)
}
