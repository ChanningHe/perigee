use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use perigee_core::ipc::{
    EventLevel, FdbRuntimeStatus, ModuleState, ModuleStatus, ProfileDetailStatus, ProfileEvent,
    ProfileState, ProfileSummary, VfRuntimeStatus, VfSnapshot,
};
use perigee_sriov::config::{FdbMode, SriovFileConfig, SriovProfileConfig};
use perigee_sriov::fdb::FdbManager;
use perigee_sriov::vf;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::module::Module;

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
    fdb_managers: Vec<FdbManager>,
    shutdown_tx: Option<broadcast::Sender<()>>,
}

impl SriovModule {
    pub fn new() -> Self {
        Self {
            profiles: BTreeMap::new(),
            fdb_managers: Vec::new(),
            shutdown_tx: None,
        }
    }

    fn load_profiles(_config: &toml::Value) -> BTreeMap<String, SriovProfileConfig> {
        let path = crate::config::sriov_config_path();
        if !path.exists() {
            return BTreeMap::new();
        }
        match SriovFileConfig::load(&path) {
            Ok(file_config) => file_config.sriov,
            Err(e) => {
                error!(error = %e, "failed to load sriov config");
                BTreeMap::new()
            }
        }
    }

    async fn apply_all_profiles(&mut self) {
        for (name, rt) in &mut self.profiles {
            let profile_name = name.clone();
            let config = rt.config.clone();

            // Run blocking sysfs/ip-link operations on a dedicated thread
            let apply_result = tokio::task::spawn_blocking(move || {
                vf::apply_profile(&profile_name, &config)
            })
            .await;

            match apply_result {
                Ok(Ok(result)) => {
                    rt.last_applied = Some(Utc::now());
                    rt.config_dirty = false;
                    if result.is_success() {
                        rt.state = ProfileState::Active;
                        rt.last_error = None;
                        push_event(&mut rt.events, EventLevel::Info, format!(
                            "Applied: {} VFs created and configured", result.configured_vfs
                        ));
                    } else if result.is_degraded() {
                        rt.state = ProfileState::Degraded;
                        rt.error_count += result.errors.len() as u32;
                        let msg = result.errors.join("; ");
                        rt.last_error = Some(msg.clone());
                        push_event(&mut rt.events, EventLevel::Warn, format!(
                            "Degraded: {}/{} VFs OK — {}", result.configured_vfs, result.total_vfs, msg
                        ));
                    } else {
                        rt.state = ProfileState::Error;
                        rt.error_count += result.errors.len() as u32;
                        let msg = result.errors.join("; ");
                        rt.last_error = Some(msg.clone());
                        push_event(&mut rt.events, EventLevel::Error, format!("Failed: {}", msg));
                    }
                }
                Ok(Err(e)) => {
                    let msg = e.to_string();
                    if msg.contains("no interface found with MAC") {
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

    fn start_fdb_watchers(&mut self, shutdown_tx: &broadcast::Sender<()>) {
        for (name, rt) in &self.profiles {
            if rt.config.fdb.mode != FdbMode::DaemonWatch {
                continue;
            }

            let pf_iface = match perigee_core::sysfs::find_iface_by_mac(&rt.config.mac.to_string()) {
                Ok(iface) => iface,
                Err(_) => continue,
            };

            let bridge = match detect_bridge_for_pf(&pf_iface) {
                Some(br) => br,
                None => {
                    info!(
                        profile = %name,
                        pf = %pf_iface,
                        "FDB DaemonWatch skipped: PF is not a bridge port (add PF to a bridge to enable FDB sync)"
                    );
                    continue;
                }
            };

            let mgr = FdbManager::new(FdbMode::DaemonWatch, pf_iface.clone(), Some(bridge.clone()));

            if let Err(e) = mgr.full_sync() {
                warn!(profile = %name, error = %e, "FDB initial sync failed");
            }

            self.fdb_managers.push(mgr);

            let shutdown_rx = shutdown_tx.subscribe();
            let watcher = FdbManager::new(FdbMode::DaemonWatch, pf_iface, Some(bridge));
            tokio::spawn(async move {
                if let Err(e) = watcher.start_watcher(shutdown_rx).await {
                    error!(error = %e, "FDB watcher error");
                }
            });
        }
    }

    pub fn get_profile_detail(&self, profile_name: &str) -> Option<ProfileDetailStatus> {
        let rt = self.profiles.get(profile_name)?;
        let pf_iface = perigee_core::sysfs::find_iface_by_mac(&rt.config.mac.to_string()).ok();

        let vfs = build_vf_status(&rt.config, pf_iface.as_deref());

        let fdb_status = self
            .fdb_managers
            .iter()
            .find(|m| {
                pf_iface
                    .as_ref()
                    .map(|iface| m.pf_dev == *iface)
                    .unwrap_or(false)
            })
            .map(|m| FdbRuntimeStatus {
                mode: format!("{:?}", m.mode),
                bridge: m.bridge.clone(),
                managed_entries: m.managed_count(),
                last_sync_secs_ago: None,
                errors: Vec::new(),
            })
            .unwrap_or(FdbRuntimeStatus {
                mode: format!("{:?}", rt.config.fdb.mode),
                bridge: None,
                managed_entries: 0,
                last_sync_secs_ago: None,
                errors: Vec::new(),
            });

        Some(ProfileDetailStatus {
            name: rt.name.clone(),
            state: rt.state,
            pf_iface,
            pf_mac: rt.config.mac.to_string(),
            last_applied: rt.last_applied,
            config_dirty: rt.config_dirty,
            vfs,
            fdb: fdb_status,
        })
    }

    pub fn get_profile_events(&self, profile_name: &str, limit: usize) -> Vec<ProfileEvent> {
        self.profiles
            .get(profile_name)
            .map(|rt| {
                let len = rt.events.len();
                let start = if len > limit { len - limit } else { 0 };
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
                    push_event(&mut rt.events, EventLevel::Warn, format!("Retry partial: {}", msg));
                }
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                rt.last_error = Some(msg.clone());
                push_event(&mut rt.events, EventLevel::Error, format!("Retry failed: {}", msg));
                Err(e)
            }
        }
    }
}

#[async_trait]
impl Module for SriovModule {
    fn name(&self) -> &str {
        "sriov"
    }

    async fn init(&mut self, config: &toml::Value) -> Result<()> {
        let profiles = Self::load_profiles(config);
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

        let (tx, _) = broadcast::channel(4);
        self.shutdown_tx = Some(tx.clone());
        self.start_fdb_watchers(&tx);

        Ok(())
    }

    async fn reload(&mut self, config: &toml::Value) -> Result<()> {
        let new_profiles = Self::load_profiles(config);

        // Remove profiles no longer in config
        self.profiles.retain(|name, _| new_profiles.contains_key(name));

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

        info!(profiles = self.profiles.len(), "SR-IOV module reloaded config");
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("SR-IOV module shutting down");
        Ok(())
    }

    fn profile_detail(&self, profile: &str) -> Option<ProfileDetailStatus> {
        self.get_profile_detail(profile)
    }

    fn profile_events(&self, profile: &str, limit: usize) -> Vec<ProfileEvent> {
        self.get_profile_events(profile, limit)
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

fn build_vf_status(
    config: &SriovProfileConfig,
    pf_iface: Option<&str>,
) -> Vec<VfRuntimeStatus> {
    let actual_states = pf_iface
        .and_then(|pf| vf::read_actual_vf_states(pf).ok())
        .unwrap_or_default();

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

        vfs.push(VfRuntimeStatus {
            index: i,
            configured,
            actual,
            matches,
            last_error: None,
        });
    }
    vfs
}

fn detect_bridge_for_pf(pf_iface: &str) -> Option<String> {
    let bridge_path = format!("/sys/class/net/{}/master", pf_iface);
    std::fs::read_link(&bridge_path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
}
