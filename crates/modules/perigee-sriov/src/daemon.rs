use crate::config::{sriov_config_path, FdbMode, SriovFileConfig, SriovProfileConfig};
use crate::fdb::{FdbService, FdbStatus, FdbTarget};
use crate::vf;
use anyhow::{Context, Result};
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
        Self {
            profiles: BTreeMap::new(),
            fdb: FdbService::new(),
            config_loaded: false,
            shutdown_tx: None,
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

    async fn apply_all_profiles(&mut self) {
        for (name, rt) in &mut self.profiles {
            let profile_name = name.clone();
            let config = rt.config.clone();

            let apply_result =
                tokio::task::spawn_blocking(move || vf::apply_profile(&profile_name, &config))
                    .await;

            match apply_result {
                Ok(Ok(result)) => {
                    rt.last_applied = Some(Utc::now());
                    rt.config_dirty = false;
                    if result.is_success() {
                        rt.state = ProfileState::Active;
                        rt.last_error = None;
                        push_event(
                            &mut rt.events,
                            EventLevel::Info,
                            format!(
                                "Applied: {} VFs created and configured",
                                result.configured_vfs
                            ),
                        );
                    } else if result.is_degraded() {
                        rt.state = ProfileState::Degraded;
                        rt.error_count += result.errors.len() as u32;
                        let msg = result.errors.join("; ");
                        rt.last_error = Some(msg.clone());
                        push_event(
                            &mut rt.events,
                            EventLevel::Warn,
                            format!(
                                "Degraded: {}/{} VFs OK — {}",
                                result.configured_vfs, result.total_vfs, msg
                            ),
                        );
                    } else {
                        rt.state = ProfileState::Error;
                        rt.error_count += result.errors.len() as u32;
                        let msg = result.errors.join("; ");
                        rt.last_error = Some(msg.clone());
                        push_event(
                            &mut rt.events,
                            EventLevel::Error,
                            format!("Failed: {}", msg),
                        );
                    }
                }
                Ok(Err(e)) => {
                    let msg = e.to_string();
                    if is_nic_offline(&e) {
                        rt.state = ProfileState::NicOffline;
                        warn!(profile = %name, "PF not found — NIC offline?");
                    } else {
                        rt.state = ProfileState::Error;
                        rt.error_count += 1;
                    }
                    rt.last_error = Some(msg.clone());
                    push_event(&mut rt.events, EventLevel::Error, msg);
                }
                Err(e) => {
                    let msg = format!("apply task panicked: {}", e);
                    rt.state = ProfileState::Error;
                    rt.error_count += 1;
                    rt.last_error = Some(msg.clone());
                    push_event(&mut rt.events, EventLevel::Error, msg);
                }
            }
        }
    }

    pub fn get_profile_detail(&self, profile_name: &str) -> Option<ProfileDetailStatus> {
        let rt = self.profiles.get(profile_name)?;
        let pf_iface = perigee_core::sysfs::find_iface_by_mac(&rt.config.mac.to_string()).ok();

        let vfs = build_vf_status(&rt.config, pf_iface.as_deref());
        let fdb = fdb_runtime_status(&rt.config.fdb.mode, self.fdb.status(profile_name));

        Some(ProfileDetailStatus {
            name: rt.name.clone(),
            state: rt.state,
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
        self.profiles
            .get(profile_name)
            .map(|rt| {
                let len = rt.events.len();
                let start = len.saturating_sub(limit);
                rt.events[start..].to_vec()
            })
            .unwrap_or_default()
    }

    pub fn retry_profile(&mut self, profile_name: &str) -> Result<()> {
        let rt = self
            .profiles
            .get_mut(profile_name)
            .context(format!("profile '{}' not found", profile_name))?;

        info!(profile = %profile_name, "retrying profile apply");
        match vf::apply_profile(profile_name, &rt.config) {
            Ok(result) => {
                rt.last_applied = Some(Utc::now());
                rt.config_dirty = false;
                if result.is_success() {
                    rt.state = ProfileState::Active;
                    rt.last_error = None;
                    push_event(&mut rt.events, EventLevel::Info, "Retry succeeded".into());
                } else {
                    let msg = result.errors.join("; ");
                    rt.last_error = Some(msg.clone());
                    push_event(
                        &mut rt.events,
                        EventLevel::Warn,
                        format!("Retry partial: {}", msg),
                    );
                }
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                if is_nic_offline(&e) {
                    rt.state = ProfileState::NicOffline;
                } else {
                    rt.state = ProfileState::Error;
                    rt.error_count += 1;
                }
                rt.last_error = Some(msg.clone());
                push_event(
                    &mut rt.events,
                    EventLevel::Error,
                    format!("Retry failed: {}", msg),
                );
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

    async fn apply(&mut self) -> Result<()> {
        self.apply_all_profiles().await;
        if self.config_loaded {
            self.sync_fdb();
        }
        Ok(())
    }

    async fn reload(&mut self, _config: &toml::Value) -> Result<()> {
        // Fail instead of loading an empty set: dropping every profile would
        // make the FDB reconciler release all entries over a config typo.
        let new_profiles = Self::read_profiles()?;

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
                state: rt.state,
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

fn push_event(events: &mut Vec<ProfileEvent>, level: EventLevel, message: String) {
    const MAX_EVENTS: usize = 200;
    events.push(ProfileEvent {
        timestamp: Utc::now(),
        level,
        message,
    });
    if events.len() > MAX_EVENTS {
        events.drain(..events.len() - MAX_EVENTS);
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
        let trust = vf_override
            .and_then(|o| o.trust)
            .unwrap_or(config.defaults.trust);
        let spoofchk = vf_override
            .and_then(|o| o.spoofchk)
            .unwrap_or(config.defaults.spoofchk);
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
            trust,
            spoofchk,
            vlan_id,
            vlan_qos,
        };

        let actual_vf = actual_states.iter().find(|a| a.index == i);
        let actual = actual_vf.map(|a| VfSnapshot {
            mac: a.mac.clone(),
            trust: a.trust,
            spoofchk: a.spoofchk,
            vlan_id: a.vlan_id,
            vlan_qos: None,
        });

        let matches = if let Some(ref act) = actual {
            let mac_ok = mac == "(auto)" || act.mac == configured.mac;
            let trust_ok = act.trust == configured.trust;
            let spoof_ok = act.spoofchk == configured.spoofchk;
            let vlan_ok = act.vlan_id == configured.vlan_id;
            mac_ok && trust_ok && spoof_ok && vlan_ok
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
