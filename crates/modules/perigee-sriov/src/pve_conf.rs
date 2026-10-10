//! Minimal readers for PVE guests: config files
//! (`/etc/pve/{qemu-server,lxc}/*.conf`) and run state.

use std::path::Path;

/// Whether the VM or container `vmid` is running.
///
/// A VM runs when its QEMU pid file points at a live process (the pid file
/// alone can be stale after an unclean shutdown). A container runs while its
/// cgroup exists; PVE places it at `lxc/<vmid>` (cgroup v2, PVE 7+).
pub fn guest_is_running(vmid: &str) -> bool {
    let qemu_alive = std::fs::read_to_string(format!("/run/qemu-server/{vmid}.pid"))
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
    qemu_alive || Path::new(&format!("/sys/fs/cgroup/lxc/{vmid}")).is_dir()
}

/// Iterate the `key: value` pairs of the live guest config, i.e. the main
/// section only.
///
/// Scanning stops at the first `[section]` header: PVE stores snapshots
/// (`[name]`), pending changes (`[PENDING]`) and cloud-init state in the same
/// file, each with its own historical device lines that must NOT be attributed
/// to the running guest.
pub fn main_section(content: &str) -> impl Iterator<Item = (&str, &str)> {
    content
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim(), value.trim()))
}

/// True for `<prefix><digits>` keys such as `net0` or `hostpci12`, not just
/// anything starting with `prefix`.
pub fn is_indexed_key(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_first_section_header() {
        let cfg = "net0: a\n[PENDING]\nnet0: b\n[snap1]\nnet0: c\n";
        let pairs: Vec<_> = main_section(cfg).collect();
        assert_eq!(pairs, vec![("net0", "a")]);
    }

    #[test]
    fn trims_keys_and_values_and_skips_non_pairs() {
        let cfg = "# comment\n  name :  vm1  \n\nnet0: x\n";
        let pairs: Vec<_> = main_section(cfg).collect();
        assert_eq!(pairs, vec![("name", "vm1"), ("net0", "x")]);
    }

    #[test]
    fn indexed_key_requires_digits() {
        assert!(is_indexed_key("net0", "net"));
        assert!(is_indexed_key("hostpci15", "hostpci"));
        assert!(!is_indexed_key("net", "net"));
        assert!(!is_indexed_key("netfoo", "net"));
        assert!(!is_indexed_key("net1x", "net"));
    }
}
