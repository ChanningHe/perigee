pub mod intel;
pub mod mellanox;

use serde::{Deserialize, Serialize};

const VENDOR_MELLANOX: &str = "0x15b3";
const VENDOR_INTEL: &str = "0x8086";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicVendor {
    Mellanox,
    Intel,
    Other,
}

impl NicVendor {
    pub fn from_vendor_id(id: &str) -> Self {
        match id.trim() {
            VENDOR_MELLANOX => Self::Mellanox,
            VENDOR_INTEL => Self::Intel,
            _ => Self::Other,
        }
    }
}

impl std::fmt::Display for NicVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mellanox => write!(f, "Mellanox"),
            Self::Intel => write!(f, "Intel"),
            Self::Other => write!(f, "Other"),
        }
    }
}

/// Vendor-specific checks to run before VF creation.
pub fn pre_create_checks(vendor: NicVendor, iface: &str, num_vfs: u32) -> Vec<VendorWarning> {
    match vendor {
        NicVendor::Mellanox => mellanox::pre_create_checks(iface, num_vfs),
        NicVendor::Intel => intel::pre_create_checks(iface, num_vfs),
        NicVendor::Other => Vec::new(),
    }
}

#[derive(Debug, Clone)]
pub struct VendorWarning {
    pub level: WarningLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningLevel {
    Info,
    Warning,
    Error,
}
