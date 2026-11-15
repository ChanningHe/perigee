//! Daemon-side retry of a RoCE default ToS write left pending by an apply
//! (RDMA device or rdma_cm configfs not there yet).
//!
//! Like the FDB reconciler: one tokio task per pending write, its outcome
//! kept in a mutex-guarded per-profile map that the daemon reads back. A task
//! exits on its own once its generation is no longer current (re-apply,
//! reload, module shutdown), at the latest one interval later.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::Utc;
use perigee_core::ipc::{EventLevel, ProfileEvent};
use tokio::time::Instant;
use tracing::{error, info};

use super::tos::{prepare, write_if_differs, PendingTos, TosAttempt, TosPaths};
use super::StepError;

/// How often a pending ToS write is retried.
pub const TOS_RETRY_INTERVAL: Duration = Duration::from_secs(5);
/// How long one retry keeps polling, counted from its start; every apply
/// that leaves the ToS pending starts a new one.
pub const TOS_RETRY_LIMIT: Duration = Duration::from_secs(600);

/// Everything the retry touches; injectable for tests.
#[derive(Clone)]
pub struct TosEnv {
    pub paths: TosPaths,
    pub load_rdma_cm: fn() -> Result<(), StepError>,
    pub interval: Duration,
    pub limit: Duration,
}

impl TosEnv {
    pub fn system() -> Self {
        Self {
            paths: TosPaths::system(),
            load_rdma_cm: super::load_rdma_cm,
            interval: TOS_RETRY_INTERVAL,
            limit: TOS_RETRY_LIMIT,
        }
    }
}

#[derive(Debug, Default)]
struct Slot {
    /// Bumped whenever the retry is replaced or ended; a task acts only while
    /// its generation is current.
    generation: u64,
    /// Final event of the current retry, once it has ended.
    outcome: Option<ProfileEvent>,
}

#[derive(Clone)]
pub struct TosRetry {
    slots: Arc<Mutex<BTreeMap<String, Slot>>>,
    env: TosEnv,
}

impl TosRetry {
    pub fn new(env: TosEnv) -> Self {
        Self {
            slots: Arc::default(),
            env,
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// End the retry for `profile`, if any, and hand back its outcome. Once
    /// this returns the old retry cannot write: an attempt holding the lock
    /// finishes first, later ones see the new generation.
    pub fn cancel(&self, profile: &str) -> Option<ProfileEvent> {
        let mut slots = self.lock();
        let slot = slots.entry(profile.to_string()).or_default();
        slot.generation += 1;
        slot.outcome.take()
    }

    /// End every retry (module shutdown).
    pub fn cancel_all(&self) {
        for slot in self.lock().values_mut() {
            slot.generation += 1;
        }
    }

    pub fn outcome(&self, profile: &str) -> Option<ProfileEvent> {
        self.lock().get(profile).and_then(|s| s.outcome.clone())
    }

    /// Whether the current retry for `profile` ended without the ToS applied.
    pub fn failed(&self, profile: &str) -> bool {
        self.outcome(profile)
            .is_some_and(|e| e.level == EventLevel::Error)
    }

    /// Replace any retry for `profile` with one for `pending` (none when
    /// `None`), so at most one task per profile is ever live. An unclaimed
    /// outcome is dropped; [`cancel`](Self::cancel) first to keep it.
    pub fn start(&self, profile: &str, pending: Option<&PendingTos>) {
        let generation = {
            let mut slots = self.lock();
            let slot = slots.entry(profile.to_string()).or_default();
            slot.generation += 1;
            slot.outcome = None;
            slot.generation
        };
        if let Some(p) = pending {
            let this = self.clone();
            tokio::spawn(this.run(profile.to_string(), p.clone(), generation));
        }
    }

    async fn run(self, profile: String, pending: PendingTos, generation: u64) {
        let deadline = Instant::now() + self.env.limit;
        loop {
            tokio::time::sleep(self.env.interval).await;
            let (this, p, pend) = (self.clone(), profile.clone(), pending.clone());
            let attempt = match tokio::task::spawn_blocking(move || {
                this.attempt(&p, &pend, generation)
            })
            .await
            {
                Ok(Some(a)) => a,
                Ok(None) => return, // replaced or cancelled
                Err(e) => TosAttempt::Failed(format!("retry attempt panicked: {e}")),
            };
            let expired = Instant::now() >= deadline;
            if let Some((level, msg)) = conclude(&attempt, expired, &pending, self.env.limit) {
                self.finish(&profile, generation, level, msg);
                return;
            }
        }
    }

    /// One attempt; `None` once no longer current. The write happens under
    /// the lock so that `cancel` cannot return while it is in flight.
    fn attempt(&self, profile: &str, pending: &PendingTos, generation: u64) -> Option<TosAttempt> {
        let current = |slots: &BTreeMap<String, Slot>| {
            slots.get(profile).map(|s| s.generation) == Some(generation)
        };
        if !current(&self.lock()) {
            return None;
        }
        // May run `modprobe rdma_cm`, so done without holding the lock.
        let ibdev = match prepare(&self.env.paths, &pending.pf, self.env.load_rdma_cm) {
            Ok(d) => d,
            Err(attempt) => return Some(attempt),
        };
        let slots = self.lock();
        current(&slots).then(|| write_if_differs(&self.env.paths, &ibdev, pending.tos))
    }

    fn finish(&self, profile: &str, generation: u64, level: EventLevel, msg: String) {
        let mut slots = self.lock();
        let Some(slot) = slots
            .get_mut(profile)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        match level {
            EventLevel::Error => error!(profile = %profile, "{msg}"),
            _ => info!(profile = %profile, "{msg}"),
        }
        slot.outcome = Some(ProfileEvent {
            timestamp: Utc::now(),
            level,
            message: msg,
        });
    }
}

/// Final event for an attempt, or `None` to keep retrying.
fn conclude(
    attempt: &TosAttempt,
    expired: bool,
    p: &PendingTos,
    limit: Duration,
) -> Option<(EventLevel, String)> {
    let (tos, pf) = (p.tos, &p.pf);
    let (level, msg) = match attempt {
        TosAttempt::Done { changed: true } => (
            EventLevel::Info,
            format!(
                "RoCE ToS {tos} applied on {pf}; RDMA connections established before now keep \
                 their old ToS until they reconnect"
            ),
        ),
        TosAttempt::Done { changed: false } => (
            EventLevel::Info,
            format!("RoCE ToS {tos} on {pf} was already set; nothing written"),
        ),
        TosAttempt::Failed(e) => (
            EventLevel::Error,
            format!("RoCE ToS {tos} not applied on {pf}: {e}"),
        ),
        TosAttempt::Pending(why) if expired => (
            EventLevel::Error,
            format!(
                "RoCE ToS {tos} not applied on {pf}: gave up after {}s: {why}",
                limit.as_secs()
            ),
        ),
        TosAttempt::Pending(_) => return None,
    };
    Some((level, msg))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::tos::test_env::*;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PROFILE: &str = "p";
    const WAIT: Duration = Duration::from_secs(3);

    fn pending() -> PendingTos {
        PendingTos {
            pf: PF.to_string(),
            tos: 106,
        }
    }

    pub(crate) fn fake_env(sys: &FakeSys, limit: Duration) -> TosEnv {
        TosEnv {
            paths: sys.paths.clone(),
            load_rdma_cm: || Err(StepError::Failed("modprobe unavailable in tests".into())),
            interval: Duration::from_millis(10),
            limit,
        }
    }

    /// Poll until `done` or `WAIT` passes.
    pub(crate) async fn wait_for(done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        done()
    }

    fn outcome_message(r: &TosRetry) -> String {
        r.outcome(PROFILE).unwrap().message
    }

    #[tokio::test]
    async fn applies_once_the_rdma_device_appears() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let r = TosRetry::new(fake_env(&sys, WAIT));
        r.start(PROFILE, Some(&pending()));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(sys.read_tos(), "0", "nothing to write before the device");
        sys.add_rdma_device();
        assert!(wait_for(|| r.outcome(PROFILE).is_some()).await);
        assert_eq!(sys.read_tos(), "106");
        assert!(!r.failed(PROFILE));
        assert!(outcome_message(&r).contains("reconnect"));
    }

    #[tokio::test]
    async fn gives_up_after_the_limit() {
        let sys = FakeSys::new();
        let r = TosRetry::new(fake_env(&sys, Duration::from_millis(50)));
        r.start(PROFILE, Some(&pending()));
        assert!(wait_for(|| r.failed(PROFILE)).await);
        let msg = outcome_message(&r);
        assert!(
            msg.contains("gave up") && msg.contains("RDMA device"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn rejected_write_fails_without_further_retries() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        sys.add_configfs_group("garbage");
        let r = TosRetry::new(fake_env(&sys, WAIT));
        r.start(PROFILE, Some(&pending()));
        assert!(wait_for(|| r.failed(PROFILE)).await);
        assert!(outcome_message(&r).contains("not applied"));
    }

    static MODPROBES: AtomicUsize = AtomicUsize::new(0);

    fn hung_modprobe() -> Result<(), StepError> {
        MODPROBES.fetch_add(1, Ordering::SeqCst);
        Err(StepError::TimedOut("`modprobe rdma_cm` timed out".into()))
    }

    #[tokio::test]
    async fn modprobe_timeout_fails_the_retry_and_is_not_repeated() {
        let sys = FakeSys::new();
        sys.add_rdma_device(); // configfs missing: modprobe is needed
        let env = TosEnv {
            load_rdma_cm: hung_modprobe,
            ..fake_env(&sys, WAIT)
        };
        let r = TosRetry::new(env);
        r.start(PROFILE, Some(&pending()));
        assert!(wait_for(|| r.failed(PROFILE)).await);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(MODPROBES.load(Ordering::SeqCst), 1);
        assert!(outcome_message(&r).contains("not retried"));
    }

    #[tokio::test]
    async fn cancelled_retry_never_writes() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let r = TosRetry::new(fake_env(&sys, WAIT));
        r.start(PROFILE, Some(&pending()));
        assert!(r.cancel(PROFILE).is_none(), "no outcome yet");
        sys.add_rdma_device();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sys.read_tos(), "0");
        assert!(r.outcome(PROFILE).is_none());
    }

    #[tokio::test]
    async fn restart_replaces_the_live_task() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let r = TosRetry::new(fake_env(&sys, WAIT));
        r.start(PROFILE, Some(&pending()));
        r.start(PROFILE, None); // without cancel: still replaces
        sys.add_rdma_device();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sys.read_tos(), "0");
    }

    #[tokio::test]
    async fn cancel_all_stops_every_retry() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let r = TosRetry::new(fake_env(&sys, WAIT));
        r.start(PROFILE, Some(&pending()));
        r.cancel_all();
        sys.add_rdma_device();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sys.read_tos(), "0");
        assert!(r.outcome(PROFILE).is_none());
    }

    #[test]
    fn conclude_keeps_waiting_only_while_pending_and_in_time() {
        let p = pending();
        let lim = Duration::from_secs(600);
        let waiting = TosAttempt::Pending("no device".into());
        assert_eq!(conclude(&waiting, false, &p, lim), None);
        let (level, msg) = conclude(&waiting, true, &p, lim).unwrap();
        assert_eq!(level, EventLevel::Error);
        assert!(msg.contains("gave up after 600s: no device"));
        let failed = TosAttempt::Failed("EINVAL".into());
        assert_eq!(
            conclude(&failed, false, &p, lim).unwrap().0,
            EventLevel::Error
        );
        let done = TosAttempt::Done { changed: true };
        assert!(conclude(&done, true, &p, lim)
            .unwrap()
            .1
            .contains("reconnect"));
        let same = TosAttempt::Done { changed: false };
        let (level, msg) = conclude(&same, false, &p, lim).unwrap();
        assert_eq!(level, EventLevel::Info);
        assert!(msg.contains("already set") && !msg.contains("reconnect"));
    }
}
