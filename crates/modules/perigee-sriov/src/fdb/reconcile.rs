//! Level-triggered FDB reconciliation: compare what the guests need with what
//! the PF holds and what perigee owns, then converge.
//!
//! Everything here is a function of its inputs plus the [`FdbHost`] side
//! effects, so the whole policy is testable with a fake host.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use tracing::warn;

use super::scan::GuestNic;
use crate::config::FdbMode;

/// Host operations the reconciler depends on.
pub trait FdbHost {
    /// Interface name of the PF with this permanent MAC.
    fn resolve_pf(&self, pf_mac: &str) -> Result<String>;
    /// Linux bridge the PF forwards into, if any.
    fn bridge_of(&self, pf_dev: &str) -> Option<String>;
    /// MACs currently in the PF's own (`self`) FDB.
    fn present_macs(&self, pf_dev: &str) -> Result<BTreeSet<String>>;
    fn add(&self, mac: &str, pf_dev: &str) -> Result<()>;
    fn del(&self, mac: &str, pf_dev: &str) -> Result<()>;
}

/// FDB entries perigee installed on one profile's PF. Only owned entries are
/// ever deleted, so foreign entries on the PF are left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owned {
    pub pf_dev: String,
    pub macs: BTreeSet<String>,
}

/// Ownership per profile name.
pub type OwnedMap = BTreeMap<String, Owned>;

/// A profile the reconciler should act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdbTarget {
    pub profile: String,
    pub pf_mac: String,
    pub mode: FdbMode,
}

/// One managed entry and the guests whose NICs need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdbEntry {
    pub mac: String,
    pub vmids: BTreeSet<String>,
}

/// Outcome of the last reconcile pass for one `daemon_watch` profile.
#[derive(Debug, Clone, Default)]
pub struct FdbStatus {
    pub pf_dev: Option<String>,
    pub bridge: Option<String>,
    pub entries: Vec<FdbEntry>,
    pub errors: Vec<String>,
    pub last_sync: Option<Instant>,
}

impl FdbStatus {
    fn failed(pf_dev: Option<String>, error: String) -> Self {
        Self {
            pf_dev,
            errors: vec![error],
            ..Self::default()
        }
    }
}

pub struct Reconciled {
    pub owned: OwnedMap,
    pub statuses: BTreeMap<String, FdbStatus>,
}

/// Converge every target and return the new ownership map and statuses.
///
/// - `daemon_watch`: sync the PF to the guests on its bridge.
/// - `hookscript`: hand over: drop ownership but keep the entries, since
///   running guests still need them until their post-stop hook removes them.
/// - `disabled`, or a profile that no longer exists: delete owned entries.
pub fn reconcile(
    host: &impl FdbHost,
    targets: &[FdbTarget],
    owned: &OwnedMap,
    guests: &[GuestNic],
) -> Reconciled {
    let mut next = OwnedMap::new();
    let mut statuses = BTreeMap::new();

    for target in targets {
        let prev = owned.get(&target.profile);
        let kept = match target.mode {
            FdbMode::DaemonWatch => {
                let (kept, status) = sync_target(host, target, prev, guests);
                statuses.insert(target.profile.clone(), status);
                kept
            }
            FdbMode::Hookscript => None,
            FdbMode::Disabled => prev.and_then(|p| release(host, &target.profile, p)),
        };
        if let Some(o) = kept {
            next.insert(target.profile.clone(), o);
        }
    }

    for (profile, prev) in owned {
        if targets.iter().all(|t| &t.profile != profile) {
            if let Some(o) = release(host, profile, prev) {
                next.insert(profile.clone(), o);
            }
        }
    }

    Reconciled {
        owned: next,
        statuses,
    }
}

/// Group the guest MACs on `bridge`. A MAC shared by several guests (e.g. a
/// clone that kept its template's MAC) stays needed while any of them has it.
pub fn desired_on_bridge(guests: &[GuestNic], bridge: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut desired: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for nic in guests.iter().filter(|n| n.bridge == bridge) {
        desired
            .entry(nic.mac.clone())
            .or_default()
            .insert(nic.vmid.clone());
    }
    desired
}

/// Sync one PF. When the PF or its bridge cannot be resolved, ownership is
/// kept untouched: the NIC may be offline or mid-reconfiguration, and deleting
/// entries then would only cause an outage once it returns.
fn sync_target(
    host: &impl FdbHost,
    target: &FdbTarget,
    prev: Option<&Owned>,
    guests: &[GuestNic],
) -> (Option<Owned>, FdbStatus) {
    let pf = match host.resolve_pf(&target.pf_mac) {
        Ok(pf) => pf,
        Err(e) => {
            let msg = format!("PF {} not found: {e:#}", target.pf_mac);
            return (prev.cloned(), FdbStatus::failed(None, msg));
        }
    };
    let Some(bridge) = host.bridge_of(&pf) else {
        let msg = format!("PF {pf} is not a Linux bridge port");
        return (prev.cloned(), FdbStatus::failed(Some(pf), msg));
    };
    let present = match host.present_macs(&pf) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("cannot read FDB of {pf}: {e:#}");
            return (prev.cloned(), FdbStatus::failed(Some(pf), msg));
        }
    };

    let desired = desired_on_bridge(guests, &bridge);
    let prev_macs = prev.map(|p| p.macs.clone()).unwrap_or_default();
    let mut errors = Vec::new();

    // Desired MACs already on the PF are claimed: they belong to guests on
    // this bridge, which is exactly the domain perigee manages. This also
    // adopts entries left by an older daemon run or a hookscript.
    let mut macs: BTreeSet<String> = desired
        .keys()
        .filter(|m| present.contains(*m))
        .cloned()
        .collect();

    for mac in desired.keys().filter(|m| !present.contains(*m)) {
        match host.add(mac, &pf) {
            Ok(()) => {
                macs.insert(mac.clone());
            }
            Err(e) => errors.push(format!("add {mac}: {e:#}")),
        }
    }

    for mac in prev_macs.iter().filter(|m| !desired.contains_key(*m)) {
        if let Err(e) = host.del(mac, &pf) {
            // Still installed: keep owning it so the next pass retries.
            errors.push(format!("del {mac}: {e:#}"));
            macs.insert(mac.clone());
        }
    }

    let entries = macs
        .iter()
        .map(|mac| FdbEntry {
            mac: mac.clone(),
            vmids: desired.get(mac).cloned().unwrap_or_default(),
        })
        .collect();
    let status = FdbStatus {
        pf_dev: Some(pf.clone()),
        bridge: Some(bridge),
        entries,
        errors,
        last_sync: Some(Instant::now()),
    };
    let kept = (!macs.is_empty()).then_some(Owned { pf_dev: pf, macs });
    (kept, status)
}

/// Delete every owned entry. Returns what could not be deleted, to retry.
fn release(host: &impl FdbHost, profile: &str, owned: &Owned) -> Option<Owned> {
    let failed: BTreeSet<String> = owned
        .macs
        .iter()
        .filter(|mac| match host.del(mac, &owned.pf_dev) {
            Ok(()) => false,
            Err(e) => {
                warn!(profile, mac = %mac, error = %e, "FDB release failed");
                true
            }
        })
        .cloned()
        .collect();
    (!failed.is_empty()).then(|| Owned {
        pf_dev: owned.pf_dev.clone(),
        macs: failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const PF_MAC: &str = "b8:ce:f6:00:00:01";
    const PF: &str = "enp1s0f0";
    const BR: &str = "vmbr0";

    /// In-memory host: one PF with a self FDB.
    struct FakeHost {
        fdb: RefCell<BTreeSet<String>>,
        bridge: Option<String>,
        pf_online: bool,
        failing: BTreeSet<String>,
    }

    impl FakeHost {
        fn new(present: &[&str]) -> Self {
            Self {
                fdb: RefCell::new(present.iter().map(|s| s.to_string()).collect()),
                bridge: Some(BR.to_string()),
                pf_online: true,
                failing: BTreeSet::new(),
            }
        }
        fn fdb(&self) -> Vec<String> {
            self.fdb.borrow().iter().cloned().collect()
        }
        fn check(&self, mac: &str) -> Result<()> {
            if self.failing.contains(mac) {
                anyhow::bail!("injected failure");
            }
            Ok(())
        }
    }

    impl FdbHost for FakeHost {
        fn resolve_pf(&self, pf_mac: &str) -> Result<String> {
            if self.pf_online && pf_mac == PF_MAC {
                Ok(PF.to_string())
            } else {
                anyhow::bail!("no such interface")
            }
        }
        fn bridge_of(&self, _pf_dev: &str) -> Option<String> {
            self.bridge.clone()
        }
        fn present_macs(&self, _pf_dev: &str) -> Result<BTreeSet<String>> {
            Ok(self.fdb.borrow().clone())
        }
        fn add(&self, mac: &str, _pf_dev: &str) -> Result<()> {
            self.check(mac)?;
            self.fdb.borrow_mut().insert(mac.to_string());
            Ok(())
        }
        fn del(&self, mac: &str, _pf_dev: &str) -> Result<()> {
            self.check(mac)?;
            self.fdb.borrow_mut().remove(mac);
            Ok(())
        }
    }

    fn target(mode: FdbMode) -> FdbTarget {
        FdbTarget {
            profile: "p".to_string(),
            pf_mac: PF_MAC.to_string(),
            mode,
        }
    }

    fn nic(vmid: &str, mac: &str, bridge: &str) -> GuestNic {
        GuestNic {
            vmid: vmid.to_string(),
            mac: mac.to_string(),
            bridge: bridge.to_string(),
        }
    }

    fn owned(macs: &[&str]) -> OwnedMap {
        OwnedMap::from([(
            "p".to_string(),
            Owned {
                pf_dev: PF.to_string(),
                macs: macs.iter().map(|s| s.to_string()).collect(),
            },
        )])
    }

    const A: &str = "52:54:00:00:00:0a";
    const B: &str = "52:54:00:00:00:0b";
    const FOREIGN: &str = "02:00:00:00:00:ff";

    #[test]
    fn adds_guest_macs_on_the_pf_bridge_only() {
        let host = FakeHost::new(&[]);
        let guests = [nic("100", A, BR), nic("101", B, "vmbr1")];
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &OwnedMap::new(),
            &guests,
        );
        assert_eq!(host.fdb(), vec![A]);
        assert_eq!(r.owned, owned(&[A]));
        assert!(r.statuses["p"].errors.is_empty());
    }

    #[test]
    fn removes_owned_mac_once_no_guest_needs_it() {
        // MAC changed / NIC deleted / NIC moved to another bridge.
        let host = FakeHost::new(&[A]);
        let guests = [nic("100", B, BR)];
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &owned(&[A]),
            &guests,
        );
        assert_eq!(host.fdb(), vec![B]);
        assert_eq!(r.owned, owned(&[B]));
    }

    #[test]
    fn never_deletes_foreign_entries() {
        let host = FakeHost::new(&[FOREIGN]);
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &OwnedMap::new(),
            &[],
        );
        assert_eq!(host.fdb(), vec![FOREIGN]);
        assert!(r.owned.is_empty());
    }

    #[test]
    fn restores_owned_entry_lost_from_kernel() {
        let host = FakeHost::new(&[]);
        let guests = [nic("100", A, BR)];
        reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &owned(&[A]),
            &guests,
        );
        assert_eq!(host.fdb(), vec![A]);
    }

    #[test]
    fn shared_mac_survives_while_any_guest_uses_it() {
        let host = FakeHost::new(&[A]);
        let guests = [nic("100", A, BR), nic("200", A, BR)];
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &owned(&[A]),
            &guests,
        );
        let entry = &r.statuses["p"].entries[0];
        assert_eq!(entry.vmids.len(), 2);

        // Guest 100 is deleted; 200 still has the MAC.
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &r.owned,
            &guests[1..],
        );
        assert_eq!(host.fdb(), vec![A]);
        assert_eq!(r.owned, owned(&[A]));
    }

    #[test]
    fn failed_delete_is_kept_for_retry() {
        let mut host = FakeHost::new(&[A]);
        host.failing.insert(A.to_string());
        let r = reconcile(&host, &[target(FdbMode::DaemonWatch)], &owned(&[A]), &[]);
        assert_eq!(r.owned, owned(&[A]));
        assert_eq!(r.statuses["p"].errors.len(), 1);
    }

    #[test]
    fn failed_add_is_reported_and_not_owned() {
        let mut host = FakeHost::new(&[]);
        host.failing.insert(A.to_string());
        let guests = [nic("100", A, BR)];
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &OwnedMap::new(),
            &guests,
        );
        assert!(r.owned.is_empty());
        assert_eq!(r.statuses["p"].errors.len(), 1);
    }

    #[test]
    fn offline_pf_keeps_ownership_and_reports() {
        let mut host = FakeHost::new(&[A]);
        host.pf_online = false;
        let r = reconcile(&host, &[target(FdbMode::DaemonWatch)], &owned(&[A]), &[]);
        assert_eq!(host.fdb(), vec![A]);
        assert_eq!(r.owned, owned(&[A]));
        assert!(r.statuses["p"].errors[0].contains("not found"));
    }

    #[test]
    fn unbridged_pf_keeps_ownership_and_reports() {
        let mut host = FakeHost::new(&[A]);
        host.bridge = None;
        let r = reconcile(&host, &[target(FdbMode::DaemonWatch)], &owned(&[A]), &[]);
        assert_eq!(r.owned, owned(&[A]));
        assert!(r.statuses["p"].errors[0].contains("bridge"));
    }

    #[test]
    fn disabled_profile_releases_its_entries() {
        let host = FakeHost::new(&[A, FOREIGN]);
        let guests = [nic("100", A, BR)];
        let r = reconcile(&host, &[target(FdbMode::Disabled)], &owned(&[A]), &guests);
        assert_eq!(host.fdb(), vec![FOREIGN]);
        assert!(r.owned.is_empty());
        assert!(r.statuses.is_empty());
    }

    #[test]
    fn removed_profile_releases_its_entries() {
        let host = FakeHost::new(&[A]);
        let r = reconcile(&host, &[], &owned(&[A]), &[]);
        assert!(host.fdb().is_empty());
        assert!(r.owned.is_empty());
    }

    #[test]
    fn hookscript_mode_hands_over_without_deleting() {
        let host = FakeHost::new(&[A]);
        let r = reconcile(&host, &[target(FdbMode::Hookscript)], &owned(&[A]), &[]);
        assert_eq!(host.fdb(), vec![A]);
        assert!(r.owned.is_empty());
    }
}
