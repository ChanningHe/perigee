use crate::error::{PerigeeError, Result};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IommuStatus {
    Enabled,
    Disabled { hint: String },
}

/// Check if IOMMU is enabled by examining /sys/class/iommu/ directory.
pub fn detect_iommu() -> IommuStatus {
    let iommu_path = Path::new("/sys/class/iommu");
    if iommu_path.exists() {
        if let Ok(entries) = fs::read_dir(iommu_path) {
            if entries.count() > 0 {
                return IommuStatus::Enabled;
            }
        }
    }

    // Fallback: check kernel command line
    if let Ok(cmdline) = fs::read_to_string("/proc/cmdline") {
        if cmdline.contains("intel_iommu=on") || cmdline.contains("amd_iommu=on") {
            return IommuStatus::Enabled;
        }
    }

    IommuStatus::Disabled {
        hint: detect_cpu_vendor_hint(),
    }
}

fn detect_cpu_vendor_hint() -> String {
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        if cpuinfo.contains("GenuineIntel") {
            return "Add 'intel_iommu=on iommu=pt' to GRUB_CMDLINE_LINUX in /etc/default/grub".to_string();
        }
        if cpuinfo.contains("AuthenticAMD") {
            return "Add 'amd_iommu=on iommu=pt' to GRUB_CMDLINE_LINUX in /etc/default/grub".to_string();
        }
    }
    "Add 'intel_iommu=on' or 'amd_iommu=on' and 'iommu=pt' to kernel parameters".to_string()
}

pub fn require_iommu() -> Result<()> {
    match detect_iommu() {
        IommuStatus::Enabled => Ok(()),
        IommuStatus::Disabled { hint } => Err(PerigeeError::Iommu(hint)),
    }
}
