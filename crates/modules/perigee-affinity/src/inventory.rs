//! Proxmox VM inventory adapter for the normalized affinity model.
//!
//! Parsing is kept separate from filesystem and `qm` access so the Proxmox
//! semantics can be tested with small configuration fixtures.

use crate::model::{CpuSet, VmCpuShape, VmOwnership, VmResource, VmStatus};
use crate::pve;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

const PVE_QEMU_DIR: &str = "/etc/pve/qemu-server";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryWarning {
    pub vmid: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InventorySnapshot {
    pub vms: Vec<VmResource>,
    pub warnings: Vec<InventoryWarning>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedVmConfig {
    pub sockets: Option<usize>,
    pub smp: Option<usize>,
    pub cores: Option<usize>,
    pub vcpus: Option<usize>,
    pub cpu_hotplug: bool,
    pub affinity: Option<CpuSet>,
    pub template: bool,
}

/// Read all visible QEMU VMs and normalize their CPU shape and affinity.
pub fn collect(exclude_vmids: &[u32]) -> Result<InventorySnapshot> {
    let listed = pve::list_vms()?;
    let mut snapshot = InventorySnapshot::default();

    for vm in listed {
        let path = PathBuf::from(format!("{PVE_QEMU_DIR}/{}.conf", vm.vmid));
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                snapshot.warnings.push(InventoryWarning {
                    vmid: vm.vmid,
                    message: format!("reading {}: {error}", path.display()),
                });
                continue;
            }
        };

        match parse_vm_config(vm.vmid, &vm.name, &vm.status, &content, exclude_vmids) {
            Ok(Some(resource)) => snapshot.vms.push(resource),
            Ok(None) => {}
            Err(error) => snapshot.warnings.push(InventoryWarning {
                vmid: vm.vmid,
                message: error.to_string(),
            }),
        }
    }

    snapshot.vms.sort_by_key(|vm| vm.vmid);
    Ok(snapshot)
}

/// Parse one `/etc/pve/qemu-server/<vmid>.conf` fixture.
///
/// Templates are intentionally omitted from the inventory. Invalid CPU shapes
/// are returned as errors rather than clamped or silently treated as zero.
pub fn parse_vm_config(
    vmid: u32,
    name: &str,
    status: &str,
    content: &str,
    exclude_vmids: &[u32],
) -> Result<Option<VmResource>> {
    if vmid == 0 {
        bail!("VMID must be greater than zero");
    }

    let config = parse_config(content)?;
    if config.template {
        return Ok(None);
    }

    let cpu = VmCpuShape::from_proxmox(
        config.sockets,
        config.smp,
        config.cores,
        config.vcpus,
        config.cpu_hotplug,
    )
    .with_context(|| format!("invalid CPU shape for VM {vmid}"))?;

    let ownership = if exclude_vmids.contains(&vmid) {
        VmOwnership::Excluded
    } else {
        VmOwnership::Managed
    };

    Ok(Some(VmResource {
        vmid,
        name: name.to_string(),
        status: parse_status(status),
        cpu,
        current_affinity: config.affinity,
        ownership,
    }))
}

fn parse_config(content: &str) -> Result<ParsedVmConfig> {
    let mut config = ParsedVmConfig::default();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "sockets" => config.sockets = Some(parse_positive(key, value)?),
            "smp" => config.smp = Some(parse_positive(key, value)?),
            "cores" => config.cores = Some(parse_positive(key, value)?),
            "vcpus" => config.vcpus = Some(parse_nonnegative(key, value)?),
            "hotplug" => config.cpu_hotplug = hotplug_has_cpu(value),
            "template" => config.template = parse_boolish(value)?,
            "affinity" => {
                if !value.is_empty() {
                    config.affinity = Some(
                        CpuSet::parse(value)
                            .with_context(|| format!("invalid affinity '{value}'"))?,
                    );
                }
            }
            _ => {}
        }
    }
    Ok(config)
}

fn parse_positive(key: &str, value: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .with_context(|| format!("invalid {key} value '{value}'"))?;
    if parsed == 0 {
        bail!("{key} must be greater than zero");
    }
    Ok(parsed)
}

fn parse_nonnegative(key: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .with_context(|| format!("invalid {key} value '{value}'"))
}

fn parse_boolish(value: &str) -> Result<bool> {
    match value {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => bail!("invalid boolean value '{value}'"),
    }
}

fn hotplug_has_cpu(value: &str) -> bool {
    value == "1" || value.split(',').any(|feature| feature.trim() == "cpu")
}

fn parse_status(status: &str) -> VmStatus {
    match status {
        "running" => VmStatus::Running,
        "stopped" => VmStatus::Stopped,
        _ => VmStatus::Unknown,
    }
}

#[allow(dead_code)]
fn config_path(vmid: u32) -> impl AsRef<Path> {
    PathBuf::from(format!("{PVE_QEMU_DIR}/{vmid}.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_cpu_shape_and_affinity() {
        let vm = parse_vm_config(
            100,
            "web",
            "running",
            "sockets: 2\ncores: 4\naffinity: 0-3,8\n",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(vm.cpu.max_vcpus, 8);
        assert_eq!(vm.cpu.active_vcpus, 8);
        assert_eq!(vm.current_affinity.unwrap().as_slice(), &[0, 1, 2, 3, 8]);
        assert_eq!(vm.status, VmStatus::Running);
        assert_eq!(vm.ownership, VmOwnership::Managed);
    }

    #[test]
    fn uses_active_vcpus_for_hotplugged_vm() {
        let vm = parse_vm_config(
            101,
            "hotplug",
            "stopped",
            "sockets: 2\ncores: 4\nvcpus: 3\nhotplug: network,disk,usb,cpu\n",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(vm.cpu.max_vcpus, 8);
        assert_eq!(vm.cpu.active_vcpus, 3);
        assert!(vm.cpu.cpu_hotplug);
    }

    #[test]
    fn legacy_smp_is_supported_and_excluded_vms_are_external() {
        let vm = parse_vm_config(
            102,
            "legacy",
            "stopped",
            "smp: 4\ncores: 2\naffinity: 4-7\n",
            &[102],
        )
        .unwrap()
        .unwrap();
        assert_eq!(vm.cpu.max_vcpus, 8);
        assert_eq!(vm.ownership, VmOwnership::Excluded);
    }

    #[test]
    fn missing_cpu_fields_use_proxmox_defaults() {
        let vm = parse_vm_config(103, "default", "stopped", "name: default\n", &[])
            .unwrap()
            .unwrap();
        assert_eq!(vm.cpu.active_vcpus, 1);
    }

    #[test]
    fn templates_are_not_inventory_resources() {
        assert!(
            parse_vm_config(104, "template", "stopped", "template: 1\n", &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn zero_vcpus_uses_the_proxmox_default() {
        let vm = parse_vm_config(
            105,
            "default-vcpus",
            "stopped",
            "sockets: 2\ncores: 2\nvcpus: 0\n",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(vm.cpu.active_vcpus, 4);
    }

    #[test]
    fn malformed_cpu_shape_is_rejected() {
        let error = parse_vm_config(
            106,
            "bad",
            "stopped",
            "sockets: 1\ncores: 2\nvcpus: 3\n",
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid CPU shape"));
    }

    #[test]
    fn malformed_affinity_is_rejected() {
        let error = parse_vm_config(107, "bad", "stopped", "affinity: 8-4\n", &[]).unwrap_err();
        assert!(error.to_string().contains("invalid affinity"));
    }
}
