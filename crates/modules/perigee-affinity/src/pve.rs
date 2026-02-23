use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

const PVE_QEMU_DIR: &str = "/etc/pve/qemu-server";

#[derive(Debug, Clone)]
pub struct Vm {
    pub vmid: u32,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct VmConfig {
    pub cores: usize,
    pub affinity: Option<String>,
}

pub fn list_vms() -> Result<Vec<Vm>> {
    let output = Command::new("qm")
        .arg("list")
        .output()
        .context("failed to run qm list")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("qm list failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut vms = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("VMID") {
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let vmid = match fields[0].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        vms.push(Vm {
            vmid,
            name: fields[1].to_string(),
            status: fields[2].to_string(),
        });
    }

    vms.sort_by_key(|v| v.vmid);
    Ok(vms)
}

pub fn set_affinity(vmid: u32, affinity: &str, dry_run: bool) -> Result<()> {
    if vmid == 0 {
        bail!("vmid must be > 0");
    }
    if affinity.trim().is_empty() {
        bail!("affinity string is empty");
    }
    if dry_run {
        return Ok(());
    }

    let output = Command::new("qm")
        .args(["set", &vmid.to_string(), "--affinity", affinity])
        .output()
        .context("failed to run qm set")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("qm set failed: {}", stderr.trim());
    }
    Ok(())
}

pub fn read_vm_config(vmid: u32) -> Result<VmConfig> {
    let path = PathBuf::from(format!("{}/{}.conf", PVE_QEMU_DIR, vmid));
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading VM config {}", path.display()))?;

    let mut config = VmConfig::default();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("cores:") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                config.cores = n;
            }
        } else if let Some(rest) = line.strip_prefix("affinity:") {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                config.affinity = Some(val);
            }
        }
    }
    Ok(config)
}

/// Read all VM configs from /etc/pve/qemu-server/*.conf
pub fn read_all_vm_configs() -> Vec<(u32, VmConfig)> {
    let dir = PathBuf::from(PVE_QEMU_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut configs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(id_str) = name.strip_suffix(".conf") {
            if let Ok(vmid) = id_str.parse::<u32>() {
                if let Ok(cfg) = read_vm_config(vmid) {
                    configs.push((vmid, cfg));
                }
            }
        }
    }
    configs.sort_by_key(|(vmid, _)| *vmid);
    configs
}
