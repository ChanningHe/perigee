//! Guest lifecycle hook for `hookscript` mode: PVE runs the generated script,
//! which calls `perigee sriov fdb-hook <vmid> <phase>` and lands here.
//!
//! It reuses the daemon's parsing, bridge resolution and kernel access, so
//! both modes agree on which MACs belong on which PF.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::kernel::SystemHost;
use super::reconcile::{FdbHost, FdbTarget};
use super::scan::{self, GuestNic};
use crate::config::{sriov_config_path, FdbMode, SriovFileConfig};
use crate::pve_conf;

const GUEST_CONF_DIRS: [&str; 2] = ["/etc/pve/qemu-server", "/etc/pve/lxc"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    PreStart,
    PostStop,
}

impl HookPhase {
    /// Phases that touch the FDB; PVE's other phases (post-start, pre-stop)
    /// need nothing.
    fn parse(phase: &str) -> Option<Self> {
        match phase {
            "pre-start" => Some(Self::PreStart),
            "post-stop" => Some(Self::PostStop),
            _ => None,
        }
    }
}

/// Handle one PVE hookscript invocation for every `hookscript`-mode profile.
pub fn run_hook(vmid: &str, phase: &str) -> Result<()> {
    if vmid.is_empty() || !vmid.bytes().all(|b| b.is_ascii_digit()) {
        bail!("invalid vmid {vmid:?}");
    }
    let Some(phase) = HookPhase::parse(phase) else {
        return Ok(());
    };

    let targets = hook_targets()?;
    if targets.is_empty() {
        return Ok(());
    }
    let Some(content) = read_guest_conf(vmid)? else {
        return Ok(());
    };
    let nics = scan::parse_nics(&content);
    let others = match phase {
        HookPhase::PreStart => Vec::new(),
        // Without knowing who else runs, deleting could cut a live guest
        // off; leaving an entry behind is harmless, so skip instead.
        HookPhase::PostStop => scan::scan_guest_nics()
            .context("cannot list other guests; FDB entries left in place")?
            .into_iter()
            .filter(|n| n.vmid != vmid && pve_conf::guest_is_running(&n.vmid))
            .collect(),
    };

    let errors = apply_hook(&SystemHost, &targets, &nics, phase, &others);
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}

fn hook_targets() -> Result<Vec<FdbTarget>> {
    let path = sriov_config_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let config = SriovFileConfig::load(&path)
        .with_context(|| format!("failed to load {}", path.display()))?;
    Ok(config
        .sriov
        .iter()
        .map(|(name, profile)| FdbTarget::new(name, profile))
        .filter(|t| t.mode == FdbMode::Hookscript)
        .collect())
}

fn read_guest_conf(vmid: &str) -> Result<Option<String>> {
    for dir in GUEST_CONF_DIRS {
        let path = Path::new(dir).join(format!("{vmid}.conf"));
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            return Ok(Some(content));
        }
    }
    Ok(None)
}

/// Add (pre-start) or remove (post-stop) the guest's MACs on every
/// `hookscript` target whose PF reaches the NIC's bridge. On post-stop a MAC
/// that a running guest in `others` still uses behind the same PF is kept.
/// Returns the errors.
fn apply_hook(
    host: &impl FdbHost,
    targets: &[FdbTarget],
    nics: &[(String, String)],
    phase: HookPhase,
    others: &[GuestNic],
) -> Vec<String> {
    let mut errors = Vec::new();
    for target in targets.iter().filter(|t| t.mode == FdbMode::Hookscript) {
        let pf = match host.resolve_pf(&target.pf_mac) {
            Ok(pf) => pf,
            Err(e) => {
                errors.push(format!("{}: PF not found: {e:#}", target.profile));
                continue;
            }
        };
        let bridges = host.bridges_of(&pf);
        let shared = |mac: &str| {
            others
                .iter()
                .any(|o| o.mac == mac && bridges.contains(&o.bridge))
        };
        for (mac, _) in nics.iter().filter(|(_, br)| bridges.contains(br)) {
            let result = match phase {
                HookPhase::PreStart => host.add(mac, &pf),
                HookPhase::PostStop if shared(mac) => Ok(()),
                HookPhase::PostStop => host.del(mac, &pf),
            };
            if let Err(e) = result {
                errors.push(format!("{}: {mac}: {e:#}", target.profile));
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fdb::fake::*;

    fn nics(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(m, b)| (m.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn pre_start_adds_and_post_stop_removes_bridge_macs() {
        let host = FakeHost::new(&[]);
        let guest = nics(&[(A, BR), (B, "vmbr9")]);
        let targets = [target(FdbMode::Hookscript)];

        assert!(apply_hook(&host, &targets, &guest, HookPhase::PreStart, &[]).is_empty());
        assert_eq!(host.fdb(), vec![A]);

        assert!(apply_hook(&host, &targets, &guest, HookPhase::PostStop, &[]).is_empty());
        assert!(host.fdb().is_empty());
    }

    #[test]
    fn post_stop_keeps_mac_still_used_by_a_running_guest() {
        // A clone kept its template's MAC and is still running on vmbr0.
        let host = FakeHost::new(&[A]);
        let targets = [target(FdbMode::Hookscript)];
        let others = [nic("200", A, BR)];
        apply_hook(
            &host,
            &targets,
            &nics(&[(A, BR)]),
            HookPhase::PostStop,
            &others,
        );
        assert_eq!(host.fdb(), vec![A]);
    }

    #[test]
    fn post_stop_ignores_sharers_on_unrelated_bridges() {
        let host = FakeHost::new(&[A]);
        let targets = [target(FdbMode::Hookscript)];
        let others = [nic("200", A, "vmbr9")];
        apply_hook(
            &host,
            &targets,
            &nics(&[(A, BR)]),
            HookPhase::PostStop,
            &others,
        );
        assert!(host.fdb().is_empty());
    }

    #[test]
    fn ignores_profiles_not_in_hookscript_mode() {
        let host = FakeHost::new(&[]);
        let targets = [target(FdbMode::DaemonWatch), target(FdbMode::Disabled)];
        apply_hook(&host, &targets, &nics(&[(A, BR)]), HookPhase::PreStart, &[]);
        assert!(host.fdb().is_empty());
    }

    #[test]
    fn reports_offline_pf() {
        let mut host = FakeHost::new(&[]);
        host.pf_online = false;
        let errors = apply_hook(
            &host,
            &[target(FdbMode::Hookscript)],
            &nics(&[(A, BR)]),
            HookPhase::PreStart,
            &[],
        );
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn only_start_and_stop_phases_act() {
        assert_eq!(HookPhase::parse("pre-start"), Some(HookPhase::PreStart));
        assert_eq!(HookPhase::parse("post-stop"), Some(HookPhase::PostStop));
        assert_eq!(HookPhase::parse("post-start"), None);
        assert_eq!(HookPhase::parse("pre-stop"), None);
    }

    #[test]
    fn rejects_non_numeric_vmid() {
        assert!(run_hook("../../etc/passwd", "pre-start").is_err());
        assert!(run_hook("", "pre-start").is_err());
    }
}
