//! The real [`FdbHost`]: sysfs lookups plus iproute2 `bridge fdb`.

use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};

use super::reconcile::{FdbHost, PfStack};

const SYS_NET: &str = "/sys/class/net";

/// Upper-device hops to follow from the PF. Real stacks are at most a few
/// deep (PF -> bond -> VLAN -> bridge -> VLAN -> bridge).
const MAX_UPPER_DEPTH: usize = 6;

const ZERO_MAC: &str = "00:00:00:00:00:00";

pub struct SystemHost;

impl FdbHost for SystemHost {
    fn resolve_pf(&self, pf_mac: &str) -> Result<String> {
        Ok(perigee_core::sysfs::find_iface_by_mac(pf_mac)?)
    }

    fn stack_of(&self, pf_dev: &str) -> PfStack {
        let uppers = reachable_uppers(pf_dev, sysfs_uppers);
        let address = |dev: &str| perigee_core::sysfs::read_iface_address(dev).ok();
        PfStack {
            bridges: uppers
                .iter()
                .filter(|dev| Path::new(SYS_NET).join(dev).join("bridge").is_dir())
                .cloned()
                .collect(),
            host_macs: host_macs(
                address(pf_dev).as_deref(),
                uppers
                    .iter()
                    .filter_map(|dev| address(dev).map(|addr| (dev.clone(), addr))),
            ),
        }
    }

    fn present_macs(&self, pf_dev: &str) -> Result<BTreeSet<String>> {
        let output = bridge(&["-j", "fdb", "show", "dev", pf_dev])?;
        if !output.status.success() {
            bail!(
                "bridge fdb show failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        parse_self_macs(&String::from_utf8_lossy(&output.stdout))
    }

    fn add(&self, mac: &str, pf_dev: &str) -> Result<()> {
        // EEXIST: the entry is already there, which is the desired state.
        bridge_fdb("add", mac, pf_dev, &["File exists"])
    }

    fn del(&self, mac: &str, pf_dev: &str) -> Result<()> {
        // ENOENT / missing device: the entry is already gone.
        bridge_fdb("del", mac, pf_dev, &["No such file", "Cannot find device"])
    }
}

/// Walk the upper-device graph from `start` and collect every device on it
/// (excluding `start`). Uppers cover both the master (bridge, bond) and
/// stacked devices (VLAN), so PF -> bond0 -> vmbr0 and PF -> PF.100 -> vmbr1
/// are both found. The walk continues through bridges, since a bridge's own
/// VLAN device can be a port of another bridge (e.g. an SDN vnet on
/// `vmbr0.100`).
fn reachable_uppers(start: &str, uppers: impl Fn(&str) -> Vec<String>) -> BTreeSet<String> {
    let mut seen = BTreeSet::from([start.to_string()]);
    let mut frontier = vec![start.to_string()];
    for _ in 0..MAX_UPPER_DEPTH {
        frontier = frontier
            .iter()
            .flat_map(|dev| uppers(dev))
            .filter(|up| seen.insert(up.clone()))
            .collect();
    }
    seen.remove(start);
    seen
}

/// Group the `(device, address)` pairs of the PF's stacked devices by MAC,
/// keeping only addresses that differ from the PF's own (`pf_addr`): the
/// embedded switch already knows that one. All-zero addresses are skipped.
fn host_macs(
    pf_addr: Option<&str>,
    devices: impl IntoIterator<Item = (String, String)>,
) -> BTreeMap<String, BTreeSet<String>> {
    let pf_addr = pf_addr.map(str::to_lowercase);
    let mut macs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (dev, addr) in devices {
        let addr = addr.to_lowercase();
        if Some(&addr) == pf_addr.as_ref() || addr == ZERO_MAC {
            continue;
        }
        macs.entry(addr).or_default().insert(dev);
    }
    macs
}

/// Upper devices of `dev`: the targets of its `upper_<name>` sysfs links.
fn sysfs_uppers(dev: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(Path::new(SYS_NET).join(dev)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_prefix("upper_"))
                .map(str::to_string)
        })
        .collect()
}

/// MACs of the `self` entries in `bridge -j fdb show` output. Entries from the
/// master bridge's software FDB (learned addresses) are not ours to manage.
fn parse_self_macs(json: &str) -> Result<BTreeSet<String>> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(json).context("parse `bridge -j fdb show` output")?;
    Ok(entries
        .iter()
        .filter(|e| {
            e["flags"]
                .as_array()
                .is_some_and(|f| f.iter().any(|v| v == "self"))
        })
        .filter_map(|e| e["mac"].as_str())
        .map(str::to_lowercase)
        .collect())
}

/// Arguments for `bridge fdb <op>`. `self` is explicit: with no flag the kernel
/// targets the master bridge's software FDB when `dev` is a bridge port, which
/// would install the guest MAC as a host-local entry instead of programming
/// the PF's embedded switch.
fn fdb_args<'a>(op: &'a str, mac: &'a str, dev: &'a str) -> [&'a str; 6] {
    ["fdb", op, mac, "dev", dev, "self"]
}

/// Run `bridge fdb <op>`, treating a stderr containing any of `benign` as
/// success.
fn bridge_fdb(op: &str, mac: &str, dev: &str, benign: &[&str]) -> Result<()> {
    let output = bridge(&fdb_args(op, mac, dev))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !benign.iter().any(|b| stderr.contains(b)) {
            bail!("bridge fdb {op} failed: {}", stderr.trim());
        }
    }
    Ok(())
}

/// Run iproute2 `bridge`. The C locale keeps stderr matching independent of
/// the host's language.
fn bridge(args: &[&str]) -> Result<Output> {
    Command::new("bridge")
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .with_context(|| format!("failed to execute `bridge {}`", args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdb_commands_target_the_device_fdb() {
        // Without `self` the kernel would program the bridge's software FDB.
        assert_eq!(
            fdb_args("add", "52:54:00:00:00:01", "enp1s0f0"),
            ["fdb", "add", "52:54:00:00:00:01", "dev", "enp1s0f0", "self"]
        );
    }

    #[test]
    fn present_macs_only_counts_self_entries() {
        let json = r#"[
            {"mac":"52:54:00:AA:BB:CC","ifname":"enp1s0f0","flags":["self"],"state":"permanent"},
            {"mac":"33:33:00:00:00:01","ifname":"enp1s0f0","flags":["self"],"state":"permanent"},
            {"mac":"52:54:00:11:22:33","ifname":"enp1s0f0","master":"vmbr0","state":""},
            {"mac":"52:54:00:44:55:66","ifname":"enp1s0f0","flags":[],"master":"vmbr0"}
        ]"#;
        let macs = parse_self_macs(json).unwrap();
        assert_eq!(
            macs.into_iter().collect::<Vec<_>>(),
            vec!["33:33:00:00:00:01", "52:54:00:aa:bb:cc"]
        );
    }

    /// Resolve bridges over a fake upper graph given as (dev, upper) edges.
    fn bridges(edges: &[(&str, &str)], bridge_devs: &[&str]) -> Vec<String> {
        let uppers = |dev: &str| {
            edges
                .iter()
                .filter(|(d, _)| *d == dev)
                .map(|(_, u)| u.to_string())
                .collect()
        };
        reachable_uppers("pf", uppers)
            .into_iter()
            .filter(|d| bridge_devs.contains(&d.as_str()))
            .collect()
    }

    fn dev(name: &str, addr: &str) -> (String, String) {
        (name.to_string(), addr.to_string())
    }

    #[test]
    fn host_macs_skip_the_pf_own_address() {
        // vmbr0 inherited the PF MAC (PVE 8 default); vmbr0.100 has its own.
        let macs = host_macs(
            Some("B8:CE:F6:00:00:01"),
            [
                dev("vmbr0", "b8:ce:f6:00:00:01"),
                dev("vmbr0.100", "02:AA:BB:CC:DD:01"),
                dev("vmbr1", "02:aa:bb:cc:dd:01"),
                dev("dummy", "00:00:00:00:00:00"),
            ],
        );
        assert_eq!(
            macs,
            BTreeMap::from([(
                "02:aa:bb:cc:dd:01".to_string(),
                ["vmbr0.100".to_string(), "vmbr1".to_string()].into()
            )])
        );
    }

    #[test]
    fn host_macs_without_pf_address_keep_everything() {
        let macs = host_macs(None, [dev("vmbr0", "b8:ce:f6:00:00:01")]);
        assert_eq!(macs.len(), 1);
    }

    #[test]
    fn finds_direct_bridge() {
        assert_eq!(bridges(&[("pf", "vmbr0")], &["vmbr0"]), vec!["vmbr0"]);
    }

    #[test]
    fn finds_bridge_through_bond() {
        let edges = [("pf", "bond0"), ("bond0", "vmbr0")];
        assert_eq!(bridges(&edges, &["vmbr0"]), vec!["vmbr0"]);
    }

    #[test]
    fn finds_bridges_through_vlan_devices() {
        let edges = [
            ("pf", "vmbr0"),
            ("pf", "pf.100"),
            ("pf.100", "vmbr1"),
            ("vmbr0", "vmbr0.200"),
            ("vmbr0.200", "vnet1"),
        ];
        assert_eq!(
            bridges(&edges, &["vmbr0", "vmbr1", "vnet1"]),
            vec!["vmbr0", "vmbr1", "vnet1"]
        );
    }

    #[test]
    fn ovs_master_is_not_a_bridge() {
        assert!(bridges(&[("pf", "ovs-system")], &[]).is_empty());
    }

    #[test]
    fn upper_cycles_terminate() {
        let edges = [("pf", "a"), ("a", "pf"), ("a", "vmbr0")];
        assert_eq!(bridges(&edges, &["vmbr0"]), vec!["vmbr0"]);
    }

    #[test]
    fn present_macs_rejects_garbage() {
        assert!(parse_self_macs("not json").is_err());
    }
}
