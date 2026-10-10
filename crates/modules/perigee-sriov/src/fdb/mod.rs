//! Host FDB management for SR-IOV PFs that are bridge ports.
//!
//! A PF's embedded switch only forwards VF traffic to MACs it knows; guests
//! behind the Linux bridge on the PF are unknown to it unless their MACs are
//! installed in the PF's own FDB. The daemon keeps that FDB converged with the
//! guest configs through a periodic, level-triggered reconcile pass (see
//! [`reconcile`]), so missed events, restarts and kernel-side loss all heal on
//! the next pass.

mod hookscript;
mod kernel;
mod reconcile;
mod scan;
mod store;

pub use hookscript::generate_hookscript;
pub use reconcile::{FdbEntry, FdbStatus, FdbTarget};

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, Notify};
use tracing::{error, info, warn};

use crate::config::FdbMode;
use reconcile::OwnedMap;

/// How often the reconciler rescans. `/etc/pve` is pmxcfs (FUSE), where
/// inotify does not deliver events, so polling is the only reliable signal.
const FDB_SYNC_INTERVAL: Duration = Duration::from_secs(15);

type Statuses = BTreeMap<String, FdbStatus>;

/// Handle to the FDB reconcile task. Targets can be replaced at any time (e.g.
/// on config reload); the task picks them up immediately.
#[derive(Default)]
pub struct FdbService {
    targets: Arc<Mutex<Vec<FdbTarget>>>,
    statuses: Arc<Mutex<Statuses>>,
    wake: Arc<Notify>,
}

impl FdbService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the profile set and trigger a reconcile pass.
    pub fn set_targets(&self, targets: Vec<FdbTarget>) {
        *self.targets.lock().unwrap() = targets;
        self.wake.notify_one();
    }

    /// Last reconcile outcome for a `daemon_watch` profile.
    pub fn status(&self, profile: &str) -> Option<FdbStatus> {
        self.statuses.lock().unwrap().get(profile).cloned()
    }

    /// Spawn the reconcile loop; it runs until `shutdown` fires. Entries are
    /// deliberately left installed on shutdown so guests keep connectivity
    /// across a daemon restart; ownership persists in [`store::OWNED_PATH`].
    pub fn spawn(&self, mut shutdown: broadcast::Receiver<()>) {
        let targets = self.targets.clone();
        let statuses = self.statuses.clone();
        let wake = self.wake.clone();

        tokio::spawn(async move {
            let mut owned = store::load(Path::new(store::OWNED_PATH));
            let mut ticker = tokio::time::interval(FDB_SYNC_INTERVAL);
            info!(
                interval_secs = FDB_SYNC_INTERVAL.as_secs(),
                "FDB reconciler started"
            );
            loop {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = wake.notified() => {}
                    _ = shutdown.recv() => break,
                }
                let snapshot = targets.lock().unwrap().clone();
                let prev_statuses = statuses.lock().unwrap().clone();
                let prev_owned = owned.clone();
                let pass = tokio::task::spawn_blocking(move || {
                    run_pass(&snapshot, &prev_owned, &prev_statuses)
                })
                .await;
                match pass {
                    Ok((next_owned, next_statuses)) => {
                        owned = next_owned;
                        *statuses.lock().unwrap() = next_statuses;
                    }
                    Err(e) => error!(error = %e, "FDB reconcile pass panicked"),
                }
            }
            info!("FDB reconciler shutting down");
        });
    }
}

/// One blocking reconcile pass over the real host.
fn run_pass(targets: &[FdbTarget], owned: &OwnedMap, prev: &Statuses) -> (OwnedMap, Statuses) {
    let watching = targets.iter().any(|t| t.mode == FdbMode::DaemonWatch);
    if !watching && owned.is_empty() {
        return (OwnedMap::new(), Statuses::new());
    }

    let guests = match scan::scan_guest_nics() {
        Ok(g) => g,
        Err(e) => {
            // Never reconcile against an unknown guest set: that would
            // release every entry. Keep state, surface the error.
            let msg = format!("PVE guest configs unavailable: {e:#}");
            if prev.values().all(|s| !s.errors.contains(&msg)) {
                warn!(error = %msg, "FDB reconcile skipped");
            }
            return (owned.clone(), with_error(targets, prev, &msg));
        }
    };

    let result = reconcile::reconcile(&kernel::SystemHost, targets, owned, &guests);
    // Log errors when they first appear, not on every pass.
    for (profile, status) in &result.statuses {
        let seen = prev
            .get(profile)
            .map(|s| s.errors.as_slice())
            .unwrap_or(&[]);
        for err in status.errors.iter().filter(|e| !seen.contains(e)) {
            warn!(profile = %profile, error = %err, "FDB reconcile");
        }
    }
    if result.owned != *owned {
        if let Err(e) = store::save(Path::new(store::OWNED_PATH), &result.owned) {
            warn!(error = format!("{e:#}"), "cannot persist FDB ownership");
        }
    }
    (result.owned, result.statuses)
}

/// Previous statuses of the watched profiles with their errors replaced.
fn with_error(targets: &[FdbTarget], prev: &Statuses, msg: &str) -> Statuses {
    targets
        .iter()
        .filter(|t| t.mode == FdbMode::DaemonWatch)
        .map(|t| {
            let status = FdbStatus {
                errors: vec![msg.to_string()],
                ..prev.get(&t.profile).cloned().unwrap_or_default()
            };
            (t.profile.clone(), status)
        })
        .collect()
}
