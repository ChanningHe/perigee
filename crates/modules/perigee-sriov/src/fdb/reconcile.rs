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
use crate::config::{FdbMode, SriovProfileConfig};

/// Host operations the reconciler depends on.
pub trait FdbHost {
    /// Interface name of the PF with this permanent MAC.
    fn resolve_pf(&self, pf_mac: &str) -> Result<String>;
    /// Linux bridges the PF forwards into, directly or through stacked devices
    /// (bond, VLAN). Empty when the PF is not behind any Linux bridge.
    fn bridges_of(&self, pf_dev: &str) -> BTreeSet<String>;
    /// MACs currently in the PF's own (`self`) FDB.
    fn present_macs(&self, pf_dev: &str) -> Result<BTreeSet<String>>;
    fn add(&self, mac: &str, pf_dev: &str) -> Result<()>;
    fn del(&self, mac: &str, pf_dev: &str) -> Result<()>;
}

/// FDB entries perigee installed on one profile's PF. Only owned entries are
/// ever deleted, so foreign entries on the PF are left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owned {
    /// Permanent MAC of the PF, used to re-resolve its current name.
    #[serde(default)]
    pub pf_mac: String,
    /// PF name at the time of the last sync (fallback when it is gone).
    pub pf_dev: String,
    pub macs: BTreeSet<String>,
    /// Owned MACs no guest needed on the last pass; deleted only if still
    /// unneeded on the next one, so a one-pass glitch (a bridge briefly gone
    /// during `ifreload`, a config mid-rewrite) never causes an outage.
    #[serde(default)]
    pub stale: BTreeSet<String>,
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

impl FdbTarget {
    pub fn new(profile: &str, config: &SriovProfileConfig) -> Self {
        Self {
            profile: profile.to_string(),
            pf_mac: config.mac.to_string(),
            mode: config.fdb.mode.clone(),
        }
    }
}

/// One managed entry and the guest NICs (VM ids, bridges) that need it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FdbEntry {
    pub mac: String,
    pub vmids: BTreeSet<String>,
    pub bridges: BTreeSet<String>,
}

/// Outcome of the last reconcile pass for one `daemon_watch` profile.
#[derive(Debug, Clone, Default)]
pub struct FdbStatus {
    pub pf_dev: Option<String>,
    pub bridges: BTreeSet<String>,
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
/// - `daemon_watch`: sync the PF to the guests on its bridges.
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
    let mut to_release: Vec<(&String, &Owned)> = Vec::new();

    for target in targets {
        let prev = owned.get(&target.profile);
        match target.mode {
            FdbMode::DaemonWatch => {
                let (kept, status) = sync_target(host, target, prev, guests);
                statuses.insert(target.profile.clone(), status);
                if let Some(o) = kept {
                    next.insert(target.profile.clone(), o);
                }
            }
            FdbMode::Hookscript => {}
            FdbMode::Disabled => to_release.extend(prev.map(|p| (&target.profile, p))),
        }
    }
    to_release.extend(
        owned
            .iter()
            .filter(|(profile, _)| targets.iter().all(|t| &t.profile != *profile)),
    );

    // Releases run after every sync so they can spare entries another profile
    // now claims on the same PF (a renamed profile, two profiles on one PF).
    let claimed: BTreeSet<(String, String)> = next
        .values()
        .flat_map(|o| o.macs.iter().map(|m| (o.pf_dev.clone(), m.clone())))
        .collect();
    for (profile, prev) in to_release {
        if let Some(o) = release(host, profile, prev, &claimed) {
            next.insert(profile.clone(), o);
        }
    }

    Reconciled {
        owned: next,
        statuses,
    }
}

/// Group the guest MACs on any of `bridges`. A MAC shared by several guests
/// (e.g. a clone that kept its template's MAC) stays needed while any of them
/// has it.
pub fn desired_on_bridges(
    guests: &[GuestNic],
    bridges: &BTreeSet<String>,
) -> BTreeMap<String, FdbEntry> {
    let mut desired: BTreeMap<String, FdbEntry> = BTreeMap::new();
    for nic in guests.iter().filter(|n| bridges.contains(&n.bridge)) {
        let entry = desired.entry(nic.mac.clone()).or_insert_with(|| FdbEntry {
            mac: nic.mac.clone(),
            ..FdbEntry::default()
        });
        entry.vmids.insert(nic.vmid.clone());
        entry.bridges.insert(nic.bridge.clone());
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
    let bridges = host.bridges_of(&pf);
    if bridges.is_empty() {
        let msg = format!("PF {pf} is not behind a Linux bridge");
        return (prev.cloned(), FdbStatus::failed(Some(pf), msg));
    }
    let present = match host.present_macs(&pf) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("cannot read FDB of {pf}: {e:#}");
            return (prev.cloned(), FdbStatus::failed(Some(pf), msg));
        }
    };

    let desired = desired_on_bridges(guests, &bridges);
    let prev = prev.cloned().unwrap_or_default();
    let mut errors = Vec::new();

    // Desired MACs already on the PF are claimed: they belong to guests on
    // the PF's bridges, which is exactly the domain perigee manages. This also
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

    let mut stale = BTreeSet::new();
    for mac in prev.macs.iter().filter(|m| !desired.contains_key(*m)) {
        if !prev.stale.contains(mac) {
            // First pass without a guest: keep it one more pass.
            stale.insert(mac.clone());
        } else if let Err(e) = host.del(mac, &pf) {
            // Still installed: keep owning it so the next pass retries.
            errors.push(format!("del {mac}: {e:#}"));
            stale.insert(mac.clone());
        }
    }
    macs.extend(stale.iter().cloned());

    // A stale MAC has no guests left.
    let entries = macs
        .iter()
        .map(|mac| {
            desired.get(mac).cloned().unwrap_or_else(|| FdbEntry {
                mac: mac.clone(),
                ..FdbEntry::default()
            })
        })
        .collect();
    let status = FdbStatus {
        pf_dev: Some(pf.clone()),
        bridges,
        entries,
        errors,
        last_sync: Some(Instant::now()),
    };
    let kept = (!macs.is_empty()).then(|| Owned {
        pf_mac: target.pf_mac.clone(),
        pf_dev: pf,
        macs,
        stale,
    });
    (kept, status)
}

/// Delete every owned entry not `claimed` (as `(pf_dev, mac)`) by another
/// profile. Returns what could not be deleted, to retry.
///
/// The PF is re-resolved by MAC because its name may have changed since the
/// entries were installed (a renamed netdev keeps its FDB); the recorded name
/// is only the fallback when the PF is absent.
fn release(
    host: &impl FdbHost,
    profile: &str,
    owned: &Owned,
    claimed: &BTreeSet<(String, String)>,
) -> Option<Owned> {
    let pf = host
        .resolve_pf(&owned.pf_mac)
        .unwrap_or_else(|_| owned.pf_dev.clone());
    let failed: BTreeSet<String> = owned
        .macs
        .iter()
        .filter(|mac| !claimed.contains(&(pf.clone(), (*mac).clone())))
        .filter(|mac| match host.del(mac, &pf) {
            Ok(()) => false,
            Err(e) => {
                warn!(profile, mac = %mac, error = %e, "FDB release failed");
                true
            }
        })
        .cloned()
        .collect();
    (!failed.is_empty()).then(|| Owned {
        pf_mac: owned.pf_mac.clone(),
        pf_dev: pf,
        macs: failed,
        ..Owned::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fdb::fake::*;

    fn owned(macs: &[&str]) -> OwnedMap {
        OwnedMap::from([(
            "p".to_string(),
            Owned {
                pf_mac: PF_MAC.to_string(),
                pf_dev: PF.to_string(),
                macs: macs.iter().map(|s| s.to_string()).collect(),
                ..Owned::default()
            },
        )])
    }

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
    fn covers_every_bridge_the_pf_reaches() {
        // e.g. PF in vmbr0 and PF.100 in vmbr1: guests on both sit behind
        // the PF's embedded switch.
        let mut host = FakeHost::new(&[]);
        host.bridges = [BR, "vmbr1"].map(String::from).into();
        let guests = [nic("100", A, BR), nic("101", B, "vmbr1")];
        let r = reconcile(
            &host,
            &[target(FdbMode::DaemonWatch)],
            &OwnedMap::new(),
            &guests,
        );
        assert_eq!(host.fdb(), vec![A, B]);
        let entry_b = &r.statuses["p"].entries[1];
        assert_eq!(entry_b.bridges, ["vmbr1".to_string()].into());
    }

    #[test]
    fn removes_owned_mac_once_no_guest_needs_it() {
        // MAC changed / NIC deleted / NIC moved to another bridge. The delete
        // waits one pass so a transient glitch cannot cause an outage.
        let host = FakeHost::new(&[A]);
        let targets = [target(FdbMode::DaemonWatch)];
        let guests = [nic("100", B, BR)];
        let r = reconcile(&host, &targets, &owned(&[A]), &guests);
        assert_eq!(host.fdb(), vec![A, B]);
        assert_eq!(r.owned["p"].stale, [A.to_string()].into());

        let r = reconcile(&host, &targets, &r.owned, &guests);
        assert_eq!(host.fdb(), vec![B]);
        assert_eq!(r.owned, owned(&[B]));
    }

    #[test]
    fn transiently_missing_guest_is_not_deleted() {
        // e.g. its bridge vanished for one pass during `ifreload -a`.
        let host = FakeHost::new(&[A]);
        let targets = [target(FdbMode::DaemonWatch)];
        let r = reconcile(&host, &targets, &owned(&[A]), &[]);
        let r = reconcile(&host, &targets, &r.owned, &[nic("100", A, BR)]);
        assert_eq!(host.fdb(), vec![A]);
        assert_eq!(r.owned, owned(&[A]));
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
        let targets = [target(FdbMode::DaemonWatch)];
        let r = reconcile(&host, &targets, &owned(&[A]), &[]);
        let r = reconcile(&host, &targets, &r.owned, &[]);
        assert!(r.owned["p"].macs.contains(A));
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
        host.bridges.clear();
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
    fn renamed_profile_keeps_entries_claimed_by_its_successor() {
        // [sriov.p] renamed to [sriov.q] on the same PF: q claims A, and the
        // release of the vanished p must not delete it.
        let host = FakeHost::new(&[A]);
        let renamed = FdbTarget {
            profile: "q".to_string(),
            ..target(FdbMode::DaemonWatch)
        };
        let guests = [nic("100", A, BR)];
        let r = reconcile(&host, &[renamed], &owned(&[A]), &guests);
        assert_eq!(host.fdb(), vec![A]);
        assert!(r.owned.contains_key("q"));
        assert!(!r.owned.contains_key("p"));
    }

    #[test]
    fn release_follows_a_renamed_pf() {
        // Recorded as "eth9" before a rename; the PF is now found by MAC.
        let host = FakeHost::new(&[A]);
        let mut prev = owned(&[A]);
        prev.get_mut("p").unwrap().pf_dev = "eth9".to_string();
        let r = reconcile(&host, &[], &prev, &[]);
        assert!(host.fdb().is_empty());
        assert!(r.owned.is_empty());
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
