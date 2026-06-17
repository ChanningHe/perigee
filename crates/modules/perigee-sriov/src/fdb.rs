use anyhow::{bail, Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::FdbMode;

const PVE_QEMU_DIR: &str = "/etc/pve/qemu-server";
const PVE_LXC_DIR: &str = "/etc/pve/lxc";

/// Managed FDB entry tracking.
#[derive(Debug, Clone)]
pub struct FdbEntry {
    pub mac: String,
    pub bridge: String,
    pub pf_dev: String,
    pub vmid: String,
}

/// FDB manager state shared across watcher tasks.
///
/// `Clone` is intentional and shallow on `entries`: the `Arc<Mutex<..>>` is
/// shared, so a cloned manager observes and mutates the SAME entry set. This
/// lets the synchronous full-sync handle and the spawned watcher task track one
/// map instead of diverging copies.
#[derive(Debug, Clone)]
pub struct FdbManager {
    pub mode: FdbMode,
    pub pf_dev: String,
    pub bridge: Option<String>,
    entries: Arc<Mutex<HashMap<String, FdbEntry>>>,
}

impl FdbManager {
    pub fn new(mode: FdbMode, pf_dev: String, bridge: Option<String>) -> Self {
        Self {
            mode,
            pf_dev,
            bridge,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn managed_count(&self) -> u32 {
        self.entries.lock().unwrap().len() as u32
    }

    /// Perform initial full sync by scanning all PVE config files.
    pub fn full_sync(&self) -> Result<()> {
        if self.mode == FdbMode::Disabled {
            return Ok(());
        }

        let bridge = self
            .bridge
            .as_deref()
            .context("bridge not configured for FDB")?;

        let mut entries = self.entries.lock().unwrap();
        entries.clear();

        for dir in &[PVE_QEMU_DIR, PVE_LXC_DIR] {
            let dir_path = Path::new(dir);
            if !dir_path.exists() {
                continue;
            }
            let read_dir = std::fs::read_dir(dir_path)?;
            for entry in read_dir {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("conf") {
                    continue;
                }
                let vmid = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                if let Ok(macs) = extract_macs_from_config(&path, bridge) {
                    for mac in macs {
                        if let Err(e) = bridge_fdb_add(&mac, &self.pf_dev) {
                            warn!(mac = %mac, error = %e, "FDB add failed during sync");
                        } else {
                            entries.insert(
                                mac.clone(),
                                FdbEntry {
                                    mac: mac.clone(),
                                    bridge: bridge.to_string(),
                                    pf_dev: self.pf_dev.clone(),
                                    vmid: vmid.clone(),
                                },
                            );
                        }
                    }
                }
            }
        }

        info!(count = entries.len(), "FDB full sync complete");
        Ok(())
    }

    /// Start the inotify watcher for daemon mode.
    pub async fn start_watcher(
        &self,
        mut shutdown: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<()> {
        if self.mode != FdbMode::DaemonWatch {
            return Ok(());
        }

        let bridge = self
            .bridge
            .as_deref()
            .context("bridge not configured")?
            .to_string();
        let pf_dev = self.pf_dev.clone();
        let entries = self.entries.clone();

        let (tx, mut rx) = mpsc::channel(100);

        let mut watcher = RecommendedWatcher::new(
            move |res: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            notify::Config::default(),
        )?;

        for dir in &[PVE_QEMU_DIR, PVE_LXC_DIR] {
            let p = Path::new(dir);
            if p.exists() {
                watcher.watch(p, RecursiveMode::NonRecursive)?;
            }
        }

        info!("FDB watcher started");

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    handle_fs_event(event, &bridge, &pf_dev, &entries);
                }
                _ = shutdown.recv() => {
                    info!("FDB watcher shutting down");
                    break;
                }
            }
        }

        Ok(())
    }
}

fn handle_fs_event(
    event: Event,
    bridge: &str,
    pf_dev: &str,
    entries: &Arc<Mutex<HashMap<String, FdbEntry>>>,
) {
    let dominated_kinds = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    if !dominated_kinds {
        return;
    }

    for path in &event.paths {
        if path.extension().and_then(|e| e.to_str()) != Some("conf") {
            continue;
        }
        let vmid = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        if matches!(event.kind, EventKind::Remove(_)) {
            let mut ents = entries.lock().unwrap();
            let to_remove: Vec<String> = ents
                .iter()
                .filter(|(_, e)| e.vmid == vmid)
                .map(|(k, _)| k.clone())
                .collect();
            for mac in to_remove {
                if let Err(e) = bridge_fdb_del(&mac, pf_dev) {
                    warn!(mac = %mac, error = %e, "FDB del failed");
                }
                ents.remove(&mac);
            }
            continue;
        }

        // Create or Modify - rescan this config file
        if let Ok(macs) = extract_macs_from_config(path, bridge) {
            let mut ents = entries.lock().unwrap();
            for mac in &macs {
                if !ents.contains_key(mac) {
                    if let Err(e) = bridge_fdb_add(mac, pf_dev) {
                        warn!(mac = %mac, vmid = %vmid, error = %e, "FDB add failed");
                    } else {
                        ents.insert(
                            mac.clone(),
                            FdbEntry {
                                mac: mac.clone(),
                                bridge: bridge.to_string(),
                                pf_dev: pf_dev.to_string(),
                                vmid: vmid.clone(),
                            },
                        );
                        info!(mac = %mac, vmid = %vmid, "FDB entry added");
                    }
                }
            }
        }
    }
}

/// Extract MAC addresses from a PVE VM/CT config file, restricted to NICs on
/// the given bridge.
fn extract_macs_from_config(path: &Path, bridge: &str) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_macs_for_bridge(&content, bridge))
}

/// Collect VM MAC addresses from PVE config text, but only for NICs attached to
/// `bridge`. A PVE net line looks like:
///   net0: virtio=52:54:00:a1:b2:c3,bridge=vmbr0,...
/// Injecting a MAC from a NIC on some other bridge into this PF's FDB would be
/// wrong, so a line's MAC is collected only when its `bridge=` matches.
fn parse_macs_for_bridge(content: &str, bridge: &str) -> Vec<String> {
    let mut macs = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("net") {
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        let value = trimmed[colon + 1..].trim();

        let mut line_mac: Option<String> = None;
        let mut line_bridge: Option<&str> = None;
        for part in value.split(',') {
            let part = part.trim();
            if let Some(br) = part.strip_prefix("bridge=") {
                line_bridge = Some(br.trim());
            } else if let Some((_model, mac)) = part.split_once('=') {
                if is_mac_address(mac) {
                    line_mac = Some(mac.to_lowercase());
                }
            }
        }

        if let (Some(mac), Some(br)) = (line_mac, line_bridge) {
            if br == bridge {
                macs.push(mac);
            }
        }
    }

    macs
}

fn is_mac_address(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return false;
    }
    parts
        .iter()
        .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn bridge_fdb_add(mac: &str, dev: &str) -> Result<()> {
    let output = std::process::Command::new("bridge")
        .args(["fdb", "add", mac, "dev", dev])
        .output()
        .context("failed to execute `bridge fdb add`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "already exists" is not a real error
        if !stderr.contains("File exists") {
            bail!("bridge fdb add failed: {}", stderr.trim());
        }
    }
    Ok(())
}

fn bridge_fdb_del(mac: &str, dev: &str) -> Result<()> {
    let output = std::process::Command::new("bridge")
        .args(["fdb", "del", mac, "dev", dev])
        .output()
        .context("failed to execute `bridge fdb del`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("No such file") {
            bail!("bridge fdb del failed: {}", stderr.trim());
        }
    }
    Ok(())
}

/// Generate a hookscript for FDB management (fallback mode).
pub fn generate_hookscript(output_path: &Path, pf_dev: &str) -> Result<()> {
    let script = format!(
        r#"#!/bin/bash
# Perigee FDB hookscript - auto-generated
# Attach to VM: qm set <vmid> --hookscript local:snippets/$(basename {output_path})

VMID="$1"
PHASE="$2"
PF_DEV="{pf_dev}"

get_vm_macs() {{
    local conf="/etc/pve/qemu-server/${{VMID}}.conf"
    [ -f "$conf" ] || conf="/etc/pve/lxc/${{VMID}}.conf"
    [ -f "$conf" ] || return
    grep -oP '(?<==)[0-9a-fA-F]{{2}}(:[0-9a-fA-F]{{2}}){{5}}' "$conf"
}}

case "$PHASE" in
    pre-start)
        for mac in $(get_vm_macs); do
            bridge fdb add "$mac" dev "$PF_DEV" 2>/dev/null
        done
        ;;
    post-stop)
        for mac in $(get_vm_macs); do
            bridge fdb del "$mac" dev "$PF_DEV" 2>/dev/null
        done
        ;;
esac
"#,
        output_path = output_path.display(),
        pf_dev = pf_dev,
    );

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, script)?;

    #[cfg(unix)]
    {
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(output_path, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    info!(path = %output_path.display(), "hookscript generated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_macs_for_bridge;

    const CONFIG: &str = "\
name: testvm
net0: virtio=52:54:00:AA:BB:CC,bridge=vmbr0,firewall=1
net1: virtio=52:54:00:11:22:33,bridge=vmbr1
scsi0: local-lvm:vm-100-disk-0,size=32G
";

    #[test]
    fn only_collects_macs_on_the_target_bridge() {
        // The NIC on vmbr1 must NOT be injected into a PF watching vmbr0.
        assert_eq!(
            parse_macs_for_bridge(CONFIG, "vmbr0"),
            vec!["52:54:00:aa:bb:cc"]
        );
        assert_eq!(
            parse_macs_for_bridge(CONFIG, "vmbr1"),
            vec!["52:54:00:11:22:33"]
        );
    }

    #[test]
    fn unrelated_bridge_yields_nothing() {
        assert!(parse_macs_for_bridge(CONFIG, "vmbr9").is_empty());
    }

    #[test]
    fn nic_without_bridge_is_skipped() {
        let cfg = "net0: virtio=52:54:00:de:ad:be\n";
        assert!(parse_macs_for_bridge(cfg, "vmbr0").is_empty());
    }
}
