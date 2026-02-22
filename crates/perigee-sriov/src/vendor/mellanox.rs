use super::{VendorWarning, WarningLevel};
use perigee_core::sysfs;

pub fn pre_create_checks(iface: &str, num_vfs: u32) -> Vec<VendorWarning> {
    let mut warnings = Vec::new();

    // Check driver is mlx5_core
    if let Ok(driver) = sysfs::read_driver_name(iface) {
        if driver != "mlx5_core" {
            warnings.push(VendorWarning {
                level: WarningLevel::Warning,
                message: format!(
                    "Mellanox NIC using driver '{}' instead of mlx5_core",
                    driver
                ),
            });
        }
    }

    // Check if mstconfig/mlxconfig is available for firmware verification
    if !command_exists("mstconfig") && !command_exists("mlxconfig") {
        warnings.push(VendorWarning {
            level: WarningLevel::Info,
            message: "mstconfig/mlxconfig not found; cannot verify firmware SR-IOV config. Install mstflint package for firmware checks.".to_string(),
        });
    }

    // Check max VFs vs firmware limit
    if let Ok(max) = sysfs::read_sriov_totalvfs(iface) {
        if num_vfs > max {
            warnings.push(VendorWarning {
                level: WarningLevel::Error,
                message: format!(
                    "requested {} VFs exceeds firmware max {} for Mellanox NIC",
                    num_vfs, max
                ),
            });
        }
    }

    warnings
}

fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
