use super::{VendorWarning, WarningLevel};
use perigee_core::sysfs;

const INTEL_DRIVERS: &[&str] = &["ixgbe", "i40e", "ice"];

pub fn pre_create_checks(iface: &str, num_vfs: u32) -> Vec<VendorWarning> {
    let mut warnings = Vec::new();

    if let Ok(driver) = sysfs::read_driver_name(iface) {
        if !INTEL_DRIVERS.contains(&driver.as_str()) {
            warnings.push(VendorWarning {
                level: WarningLevel::Warning,
                message: format!(
                    "Intel NIC using unexpected driver '{}' (expected one of: {})",
                    driver,
                    INTEL_DRIVERS.join(", ")
                ),
            });
        }

        // ixgbe driver may need reload to change VF count
        if driver == "ixgbe" {
            warnings.push(VendorWarning {
                level: WarningLevel::Info,
                message: "ixgbe driver may require driver reload to change VF count".to_string(),
            });
        }
    }

    if let Ok(max) = sysfs::read_sriov_totalvfs(iface) {
        if num_vfs > max {
            warnings.push(VendorWarning {
                level: WarningLevel::Error,
                message: format!(
                    "requested {} VFs exceeds max {} for Intel NIC",
                    num_vfs, max
                ),
            });
        }
    }

    warnings
}
