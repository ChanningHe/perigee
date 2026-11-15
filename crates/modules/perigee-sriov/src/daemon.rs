use crate::config::{sriov_config_path, FdbMode, SriovFileConfig, SriovProfileConfig};
use crate::fdb::{FdbService, FdbStatus, FdbTarget};
use crate::roce::retry::{TosEnv, TosRetry};
use crate::vf;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use perigee_core::error::PerigeeError;
use perigee_core::ipc::{
    EventLevel, FdbEntryInfo, FdbRuntimeStatus, ModuleState, ModuleStatus, ProfileDetailStatus,
    ProfileEvent, ProfileState, ProfileSummary, VfRuntimeStatus, VfSnapshot,
};
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use perigee_daemon::module::{BootPhase, Module};

/// Applies one profile; [`vf::apply_profile`] outside tests.
type ApplyFn = fn(&str, &SriovProfileConfig) -> Result<vf::ApplyResult>;

/// Events kept per profile.
const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone)]
struct ProfileRuntime {
    name: String,
    config: SriovProfileConfig,
    state: ProfileState,
    error_count: u32,
    last_applied: Option<chrono::DateTime<Utc>>,
    last_error: Option<String>,
    events: Vec<ProfileEvent>,
    config_dirty: bool,
}

pub struct SriovModule {
    profiles: BTreeMap<String, ProfileRuntime>,
    fdb: FdbService,
    /// Background retries of RoCE ToS writes an apply left pending.
    tos_retry: TosRetry,
    apply_fn: ApplyFn,
    /// sriov.toml loaded successfully at least once (gates FDB reconciling).
    config_loaded: bool,
    shutdown_tx: Option<broadcast::Sender<()>>,
}

impl Default for SriovModule {
    fn default() -> Self {
        Self::new()
    }
}

impl SriovModule {
    pub fn new() -> Self {
        Self::with_tos_env(TosEnv::system())
    }

    fn with_tos_env(env: TosEnv) -> Self {
        Self {
            profiles: BTreeMap::new(),
            fdb: FdbService::new(),
            tos_retry: TosRetry::new(env),
            apply_fn: vf::apply_profile,
            config_loaded: false,
            shutdown_tx: None,
        }
    }

    /// A failed background ToS retry degrades an otherwise healthy profile.
    fn effective_state(&self, rt: &ProfileRuntime) -> ProfileState {
        if rt.state == ProfileState::Active && self.tos_retry.failed(&rt.name) {
            ProfileState::Degraded
        } else {
            rt.state
        }
    }

    /// Stop ToS retries a reload made stale: profile removed, or its PF or
    /// enabled `[roce]` section changed, so a disabled section is never
    /// written late. Other retries keep running.
    fn cancel_stale_tos_retries(&mut self, new: &BTreeMap<String, SriovProfileConfig>) {
        for (name, rt) in &mut self.profiles {
            let stale = new.get(name).is_none_or(|cfg| {
                cfg.mac != rt.config.mac || cfg.enabled_roce() != rt.config.enabled_roce()
            });
            if stale {
                push_events(&mut rt.events, self.tos_retry.cancel(name));
            }
        }
    }

    fn fdb_targets(&self) -> Vec<FdbTarget> {
        self.profiles
            .values()
            .map(|rt| FdbTarget::new(&rt.name, &rt.config))
            .collect()
    }

    /// Push the current profiles to the FDB reconciler, starting it on first
    /// use. Only called once sriov.toml has loaded: reconciling against an
    /// empty profile set from a broken config would release every entry.
    fn sync_fdb(&mut self) {
        self.fdb.set_targets(self.fdb_targets());
        if self.shutdown_tx.is_none() {
            let (tx, _) = broadcast::channel(4);
            self.fdb.spawn(tx.subscribe());
            self.shutdown_tx = Some(tx);
        }
    }

    /// Read sriov.toml. A missing file means no profiles.
    fn read_profiles() -> Result<BTreeMap<String, SriovProfileConfig>> {
        let path = sriov_config_path();
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        SriovFileConfig::load(&path)
            .map(|file_config| file_config.sriov)
            .with_context(|| format!("failed to load {}", path.display()))
    }

    /// Apply every profile; returns the profiles whose VF configuration was
    /// cut short by a hung `ip`.
    async fn apply_all_profiles(&mut self) -> Vec<String> {
        let apply = self.apply_fn;
        let mut hung = Vec::new();
        for (name, rt) in &mut self.profiles {
            let profile_name = name.clone();
            let config = rt.config.clone();
            push_events(&mut rt.events, self.tos_retry.cancel(name));

            let apply_result =
                tokio::task::spawn_blocking(move || apply(&profile_name, &config)).await;

            match apply_result {
                Ok(Ok(result)) => {
                    self.tos_retry.start(name, result.pending_tos.as_ref());
                    record_result(rt, &result);
                    if result.vf_config_hung {
                        hung.push(name.clone());
                    }
                }
                Ok(Err(e)) => record_error(rt, &e),
                Err(e) => {
                    let msg = format!("apply task panicked: {}", e);
                    rt.state = ProfileState::Error;
                    rt.error_count += 1;
                    rt.last_error = Some(msg.clone());
                    push_event(&mut rt.events, EventLevel::Error, msg);
                }
            }
        }
        hung
    }

    pub fn get_profile_detail(&self, profile_name: &str) -> Option<ProfileDetailStatus> {
        let rt = self.profiles.get(profile_name)?;
        let pf_iface = perigee_core::sysfs::find_iface_by_mac(&rt.config.mac.to_string()).ok();

        let vfs = build_vf_status(&rt.config, pf_iface.as_deref());
        let fdb = fdb_runtime_status(&rt.config.fdb.mode, self.fdb.status(profile_name));

        Some(ProfileDetailStatus {
            name: rt.name.clone(),
            state: self.effective_state(rt),
            pf_iface,
            pf_mac: rt.config.mac.to_string(),
            last_applied: rt.last_applied,
            config_dirty: rt.config_dirty,
            vfs,
            fdb,
        })
    }

    pub fn get_fdb_entries(&self, profile_name: &str) -> Vec<FdbEntryInfo> {
        let Some(status) = self.fdb.status(profile_name) else {
            return Vec::new();
        };
        status
            .entries
            .into_iter()
            .map(|e| FdbEntryInfo {
                mac: e.mac,
                vmid: join(e.vmids),
                bridge: join(e.bridges),
            })
            .collect()
    }

    pub fn get_profile_events(&self, profile_name: &str, limit: usize) -> Vec<ProfileEvent> {
        let Some(rt) = self.profiles.get(profile_name) else {
            return Vec::new();
        };
        let mut events = rt.events.clone();
        events.extend(self.tos_retry.outcome(profile_name));
        events.sort_by_key(|e| e.timestamp);
        let start = events.len().saturating_sub(limit.min(MAX_EVENTS));
        events.split_off(start)
    }

    pub fn retry_profile(&mut self, profile_name: &str) -> Result<()> {
        self.retry_profile_with(profile_name, self.apply_fn)
    }

    fn retry_profile_with(
        &mut self,
        profile_name: &str,
        apply: impl FnOnce(&str, &SriovProfileConfig) -> Result<vf::ApplyResult>,
    ) -> Result<()> {
        let rt = self
            .profiles
            .get_mut(profile_name)
            .context(format!("profile '{}' not found", profile_name))?;

        info!(profile = %profile_name, "retrying profile apply");
        push_events(&mut rt.events, self.tos_retry.cancel(profile_name));
        match apply(profile_name, &rt.config) {
            Ok(result) => {
                self.tos_retry
                    .start(profile_name, result.pending_tos.as_ref());
                record_result(rt, &result);
                Ok(())
            }
            Err(e) => {
                record_error(rt, &e);
                Err(e)
            }
        }
    }
}

/// Classify an apply error: true when it stems from the PF MAC not matching any
/// present interface, i.e. the NIC is offline/absent rather than a real failure.
/// Matches the typed `PerigeeError::InterfaceNotFound` anywhere in the error
/// chain, so wrapping the error with `.context(...)` does not hide it.
fn is_nic_offline(e: &anyhow::Error) -> bool {
    e.downcast_ref::<PerigeeError>()
        .is_some_and(|pe| matches!(pe, PerigeeError::InterfaceNotFound(_)))
}

#[async_trait]
impl Module for SriovModule {
    fn name(&self) -> &str {
        "sriov"
    }

    /// VFs must exist before networking starts: ifupdown may configure them.
    fn boot_phase(&self) -> BootPhase {
        BootPhase::PreNetwork
    }

    async fn init(&mut self, _config: &toml::Value) -> Result<()> {
        let profiles = match Self::read_profiles() {
            Ok(p) => {
                self.config_loaded = true;
                p
            }
            Err(e) => {
                error!(
                    error = format!("{e:#}"),
                    "failed to load sriov config; FDB management paused until a successful reload"
                );
                BTreeMap::new()
            }
        };
        for (name, cfg) in profiles {
            self.profiles.insert(
                name.clone(),
                ProfileRuntime {
                    name,
                    config: cfg,
                    state: ProfileState::Pending,
                    error_count: 0,
                    last_applied: None,
                    last_error: None,
                    events: Vec::new(),
                    config_dirty: false,
                },
            );
        }
        info!(profiles = self.profiles.len(), "SR-IOV module initialized");
        Ok(())
    }

    /// Boot-time apply (the only caller). A VF configuration cut short by a
    /// hung `ip` is an error so the daemon exits before READY and systemd
    /// retries, as a hang that outlasted TimeoutStartSec did before command
    /// timeouts existed. Live applies (IPC, retry) never take this path.
    async fn apply(&mut self) -> Result<()> {
        let hung = self.apply_all_profiles().await;
        if self.config_loaded {
            self.sync_fdb();
        }
        if !hung.is_empty() {
            bail!(
                "VF configuration of profile(s) {} cut short by a hung `ip` command",
                hung.join(", ")
            );
        }
        Ok(())
    }

    async fn reload(&mut self, _config: &toml::Value) -> Result<()> {
        // Fail instead of loading an empty set: dropping every profile would
        // make the FDB reconciler release all entries over a config typo.
        let new_profiles = Self::read_profiles()?;
        self.cancel_stale_tos_retries(&new_profiles);

        self.profiles
            .retain(|name, _| new_profiles.contains_key(name));

        for (name, cfg) in new_profiles {
            if let Some(rt) = self.profiles.get_mut(&name) {
                rt.config_dirty = true;
                rt.config = cfg;
            } else {
                self.profiles.insert(
                    name.clone(),
                    ProfileRuntime {
                        name,
                        config: cfg,
                        state: ProfileState::Pending,
                        error_count: 0,
                        last_applied: None,
                        last_error: None,
                        events: Vec::new(),
                        config_dirty: true,
                    },
                );
            }
        }

        self.config_loaded = true;
        self.sync_fdb();

        info!(
            profiles = self.profiles.len(),
            "SR-IOV module reloaded config"
        );
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("SR-IOV module shutting down");
        self.tos_retry.cancel_all();
        // Stop the FDB reconciler. Installed entries stay so guests keep
        // connectivity across a daemon restart.
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(());
        }
        Ok(())
    }

    fn profile_detail(&self, profile: &str) -> Option<ProfileDetailStatus> {
        self.get_profile_detail(profile)
    }

    fn profile_events(&self, profile: &str, limit: usize) -> Vec<ProfileEvent> {
        self.get_profile_events(profile, limit)
    }

    fn fdb_entries(&self, profile: &str) -> Vec<FdbEntryInfo> {
        self.get_fdb_entries(profile)
    }

    fn retry_profile(&mut self, profile: &str) -> Result<()> {
        self.retry_profile(profile)
    }

    fn status(&self) -> ModuleStatus {
        let profiles: Vec<ProfileSummary> = self
            .profiles
            .values()
            .map(|rt| ProfileSummary {
                name: rt.name.clone(),
                state: self.effective_state(rt),
                error_count: rt.error_count,
            })
            .collect();

        let state = if profiles.is_empty() {
            ModuleState::Stopped
        } else if profiles.iter().all(|p| p.state == ProfileState::Active) {
            ModuleState::Running
        } else if profiles.iter().any(|p| p.state == ProfileState::Error) {
            ModuleState::Error
        } else {
            ModuleState::Degraded
        };

        ModuleStatus {
            name: "sriov".to_string(),
            state,
            profiles,
        }
    }
}

/// Record an apply result: state, error count, last error and events.
fn record_result(rt: &mut ProfileRuntime, result: &vf::ApplyResult) {
    rt.last_applied = Some(Utc::now());
    rt.config_dirty = false;
    push_warnings(&mut rt.events, &result.warnings);
    let msg = result.errors.join("; ");
    let (state, level, text) = if result.is_success() {
        let text = format!(
            "Applied: {} VFs created and configured",
            result.configured_vfs
        );
        (ProfileState::Active, EventLevel::Info, text)
    } else if result.is_degraded() {
        let text = format!(
            "Degraded: {}/{} VFs OK — {}",
            result.configured_vfs, result.total_vfs, msg
        );
        (ProfileState::Degraded, EventLevel::Warn, text)
    } else {
        let text = format!("Failed: {}", msg);
        (ProfileState::Error, EventLevel::Error, text)
    };
    rt.state = state;
    if state == ProfileState::Active {
        rt.last_error = None;
    } else {
        rt.error_count += result.errors.len() as u32;
        rt.last_error = Some(msg);
    }
    push_event(&mut rt.events, level, text);
}

/// Record an apply that failed before any result.
fn record_error(rt: &mut ProfileRuntime, e: &anyhow::Error) {
    let msg = e.to_string();
    if is_nic_offline(e) {
        rt.state = ProfileState::NicOffline;
        warn!(profile = %rt.name, "PF not found — NIC offline?");
    } else {
        rt.state = ProfileState::Error;
        rt.error_count += 1;
    }
    rt.last_error = Some(msg.clone());
    push_event(&mut rt.events, EventLevel::Error, msg);
}

fn push_event(events: &mut Vec<ProfileEvent>, level: EventLevel, message: String) {
    let event = ProfileEvent {
        timestamp: Utc::now(),
        level,
        message,
    };
    push_events(events, Some(event));
}

/// Append keeping the newest `MAX_EVENTS`.
fn push_events(events: &mut Vec<ProfileEvent>, new: impl IntoIterator<Item = ProfileEvent>) {
    events.extend(new);
    if events.len() > MAX_EVENTS {
        events.drain(..events.len() - MAX_EVENTS);
    }
}

fn push_warnings(events: &mut Vec<ProfileEvent>, warnings: &[String]) {
    for w in warnings {
        push_event(events, EventLevel::Warn, w.clone());
    }
}

fn build_vf_status(config: &SriovProfileConfig, pf_iface: Option<&str>) -> Vec<VfRuntimeStatus> {
    let actual_states = pf_iface
        .and_then(|pf| vf::read_actual_vf_states(pf).ok())
        .unwrap_or_default();

    // Which VM (if any) passes each VF through, and whether it is running.
    let vf_users = crate::vm_usage::scan_vf_users();

    let mut vfs = Vec::new();
    for i in 0..config.num_vfs {
        let vf_override = config.vf.iter().find(|o| o.index == i);
        let flags = vf::vf_flags(i, config);
        let mac = vf_override
            .and_then(|o| o.mac.as_ref())
            .map(|m| m.to_string())
            .unwrap_or_else(|| "(auto)".to_string());
        let vlan_id = vf_override
            .and_then(|o| o.vlan.as_ref())
            .or(config.defaults.vlan.as_ref())
            .map(|v| v.id);
        let vlan_qos = vf_override
            .and_then(|o| o.vlan.as_ref())
            .or(config.defaults.vlan.as_ref())
            .and_then(|v| v.qos);

        let configured = VfSnapshot {
            mac: mac.clone(),
            trust: flags.trust,
            spoofchk: flags.spoofchk,
            vlan_id,
            vlan_qos,
            link_state: flags.link_state.map(|s| s.to_string()),
        };

        let actual_vf = actual_states.iter().find(|a| a.index == i);
        let actual = actual_vf.map(|a| VfSnapshot {
            mac: a.mac.clone(),
            trust: a.trust,
            spoofchk: a.spoofchk,
            vlan_id: a.vlan_id,
            vlan_qos: None,
            link_state: a.link_state.map(|s| s.to_string()),
        });

        let matches = if let Some(ref act) = actual {
            let mac_ok = mac == "(auto)" || act.mac == configured.mac;
            let trust_ok = act.trust == configured.trust;
            let spoof_ok = act.spoofchk == configured.spoofchk;
            let vlan_ok = act.vlan_id == configured.vlan_id;
            // Unmanaged link state, or a driver that does not report it,
            // cannot mismatch.
            let link_ok = configured.link_state.is_none()
                || act.link_state.is_none()
                || act.link_state == configured.link_state;
            mac_ok && trust_ok && spoof_ok && vlan_ok && link_ok
        } else {
            false
        };

        let pci_addr = pf_iface.and_then(|pf| perigee_core::sysfs::read_vf_pci_addr(pf, i));
        let used_by = pci_addr
            .as_deref()
            .and_then(|p| vf_users.get(&crate::vm_usage::normalize_pci(p)).cloned());

        vfs.push(VfRuntimeStatus {
            index: i,
            pci_addr,
            used_by,
            configured,
            actual,
            matches,
            last_error: None,
        });
    }
    vfs
}

fn join(items: impl IntoIterator<Item = String>) -> String {
    items.into_iter().collect::<Vec<_>>().join(",")
}

/// IPC view of a profile's FDB state. Non-`daemon_watch` profiles have no
/// reconciler status and report just their mode.
fn fdb_runtime_status(mode: &FdbMode, status: Option<FdbStatus>) -> FdbRuntimeStatus {
    let status = status.unwrap_or_default();
    FdbRuntimeStatus {
        mode: format!("{:?}", mode),
        bridge: (!status.bridges.is_empty()).then(|| join(status.bridges)),
        managed_entries: status.entries.len() as u32,
        last_sync_secs_ago: status.last_sync.map(|t| t.elapsed().as_secs()),
        errors: status.errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roce::retry::tests::{fake_env, wait_for};
    use crate::roce::tos::test_env::{FakeSys, PF};
    use crate::roce::tos::PendingTos;
    use std::time::Duration;

    const SETTLE: Duration = Duration::from_millis(100);

    fn profile_config(roce_enabled: bool) -> SriovProfileConfig {
        let text = format!(
            "[sriov.p]\nmac = \"8c:2a:8e:88:9a:f6\"\nnum_vfs = 2\n\
             [sriov.p.roce]\nenabled = {roce_enabled}\n"
        );
        toml::from_str::<SriovFileConfig>(&text).unwrap().sriov["p"].clone()
    }

    fn pending() -> PendingTos {
        PendingTos {
            pf: PF.into(),
            tos: 106,
            skip_modprobe: false,
        }
    }

    /// One Active profile "p" whose ToS retry runs against `sys`.
    fn module(sys: &FakeSys) -> SriovModule {
        let mut m = SriovModule::with_tos_env(fake_env(sys, Duration::from_secs(3)));
        let rt = ProfileRuntime {
            name: "p".into(),
            config: profile_config(true),
            state: ProfileState::Active,
            error_count: 0,
            last_applied: None,
            last_error: None,
            events: Vec::new(),
            config_dirty: false,
        };
        m.profiles.insert("p".into(), rt);
        m
    }

    fn result(
        configured: u32,
        errors: &[&str],
        pending_tos: Option<PendingTos>,
    ) -> vf::ApplyResult {
        vf::ApplyResult {
            pf_iface: PF.into(),
            total_vfs: 2,
            created_vfs: 2,
            configured_vfs: configured,
            errors: errors.iter().map(|e| e.to_string()).collect(),
            warnings: Vec::new(),
            pending_tos,
            vf_config_hung: false,
        }
    }

    fn state(m: &SriovModule) -> ProfileState {
        m.status().profiles[0].state
    }

    #[tokio::test]
    async fn reload_disabling_roce_cancels_the_tos_retry() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let mut m = module(&sys);
        m.tos_retry.start("p", Some(&pending()));
        let live = m.tos_retry.generation("p");
        let new = BTreeMap::from([("p".to_string(), profile_config(false))]);
        m.cancel_stale_tos_retries(&new);
        // The running task only writes while its generation is current.
        assert_ne!(m.tos_retry.generation("p"), live, "retry cancelled");
        sys.add_rdma_device();
        tokio::time::sleep(SETTLE).await;
        assert_eq!(sys.read_tos(), "0", "disabled section never written late");
    }

    #[tokio::test]
    async fn reload_with_unchanged_roce_keeps_the_tos_retry_live() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let mut m = module(&sys);
        m.tos_retry.start("p", Some(&pending()));
        let live = m.tos_retry.generation("p");
        let new = BTreeMap::from([("p".to_string(), profile_config(true))]);
        m.cancel_stale_tos_retries(&new);
        assert_eq!(m.tos_retry.generation("p"), live, "same task still current");
        sys.add_rdma_device();
        assert!(wait_for(|| m.tos_retry.outcome("p").is_some()).await);
        assert_eq!(sys.read_tos(), "106", "the original task still wrote");
        m.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_tos_retry_shows_the_profile_degraded() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        sys.add_configfs_group("garbage");
        let m = module(&sys);
        m.tos_retry.start("p", Some(&pending()));
        assert!(wait_for(|| m.tos_retry.failed("p")).await);
        assert_eq!(state(&m), ProfileState::Degraded);
        let events = m.get_profile_events("p", 10);
        assert_eq!(events.last().unwrap().level, EventLevel::Error);
    }

    #[tokio::test]
    async fn retry_with_partial_result_is_degraded_even_after_cancelling_the_tos_retry() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        sys.add_configfs_group("garbage");
        let mut m = module(&sys);
        m.tos_retry.start("p", Some(&pending()));
        assert!(wait_for(|| m.tos_retry.failed("p")).await);

        let tos_err = "roce: RoCE default ToS not applied: bad value";
        m.retry_profile_with("p", |_, _| Ok(result(2, &[tos_err], None)))
            .unwrap();
        assert_eq!(state(&m), ProfileState::Degraded);
        let rt = &m.profiles["p"];
        assert_eq!(rt.last_error.as_deref(), Some(tos_err));
        assert!(
            rt.events
                .iter()
                .any(|e| e.message.contains("not applied on")),
            "the old retry's outcome moved into the profile events"
        );
    }

    #[tokio::test]
    async fn successful_retry_with_pending_tos_starts_a_new_retry() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let mut m = module(&sys);
        m.profiles.get_mut("p").unwrap().state = ProfileState::Error;
        m.retry_profile_with("p", |_, _| Ok(result(2, &[], Some(pending()))))
            .unwrap();
        assert_eq!(state(&m), ProfileState::Active);
        sys.add_rdma_device();
        assert!(wait_for(|| m.tos_retry.outcome("p").is_some()).await);
        assert_eq!(sys.read_tos(), "106");
        m.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn boot_apply_fails_when_vf_configuration_hung() {
        let sys = FakeSys::new();
        let mut m = module(&sys);
        m.apply_fn = |_, _| {
            let mut r = result(1, &["VF#1 config failed: `ip ...` did not finish"], None);
            r.vf_config_hung = true;
            Ok(r)
        };
        let err = m.apply().await.unwrap_err();
        assert!(err.to_string().contains("hung"), "{err}");
        let rt = &m.profiles["p"];
        assert_eq!(rt.state, ProfileState::Degraded, "state still recorded");
        assert!(rt.events.iter().any(|e| e.message.contains("Degraded")));
    }

    #[tokio::test]
    async fn boot_apply_succeeds_despite_ordinary_failures_and_pf_step_timeouts() {
        let sys = FakeSys::new();
        let mut m = module(&sys);
        m.apply_fn = |_, _| {
            Ok(result(
                1,
                &[
                    "VF#1 config failed: RTNETLINK answers: Invalid argument",
                    "roce: `dcb app add ...` did not finish within 10s and was killed",
                    "rx_ring: cannot read the RX ring: `ethtool -g p` did not finish",
                ],
                None,
            ))
        };
        m.apply().await.unwrap();
        assert_eq!(state(&m), ProfileState::Degraded);
    }

    #[tokio::test]
    async fn live_retry_with_hung_vf_configuration_only_reports() {
        let sys = FakeSys::new();
        let mut m = module(&sys);
        let hung = |_: &str, _: &SriovProfileConfig| {
            let mut r = result(0, &["`ip link set p up` did not finish"], None);
            r.vf_config_hung = true;
            Ok(r)
        };
        m.retry_profile_with("p", hung).unwrap();
        assert_eq!(state(&m), ProfileState::Error);
    }

    #[test]
    fn apply_results_map_to_states() {
        let sys = FakeSys::new();
        let mut m = module(&sys);
        let rt = m.profiles.get_mut("p").unwrap();
        record_result(rt, &result(0, &["boom"], None));
        assert_eq!(rt.state, ProfileState::Error);
        record_result(rt, &result(1, &["VF#1 config failed"], None));
        assert_eq!(rt.state, ProfileState::Degraded);
        record_result(rt, &result(2, &[], None));
        assert_eq!(
            (rt.state, rt.last_error.as_deref()),
            (ProfileState::Active, None)
        );
        assert_eq!(rt.error_count, 2);
    }

    #[test]
    fn nic_offline_detected_through_context_layer() {
        // apply_profile wraps the find-iface error with .context(...). The fix
        // must still recognise the inner typed error; the old string match on
        // the outermost message did not.
        let err = Err::<(), _>(PerigeeError::InterfaceNotFound("aa:bb:cc:dd:ee:ff".into()))
            .context("cannot locate PF with MAC aa:bb:cc:dd:ee:ff")
            .unwrap_err();
        assert!(is_nic_offline(&err));
    }

    #[test]
    fn other_sysfs_errors_are_not_nic_offline() {
        let err = Err::<(), _>(PerigeeError::Sysfs("permission denied".into()))
            .context("cannot locate PF")
            .unwrap_err();
        assert!(!is_nic_offline(&err));
    }
}
