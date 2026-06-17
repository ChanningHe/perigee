use async_trait::async_trait;
use perigee_core::ipc::{ModuleState, ModuleStatus};
use tracing::{error, info, warn};

use crate::affinity;
use crate::config::{AffinityConfig, AffinityFileConfig};
use crate::pve;
use crate::topology::{self, CpuTopology};

pub struct AffinityModule {
    config: AffinityConfig,
    topology: Option<CpuTopology>,
    applied_count: usize,
    last_error: Option<String>,
}

impl Default for AffinityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl AffinityModule {
    pub fn new() -> Self {
        Self {
            config: AffinityConfig::default(),
            topology: None,
            applied_count: 0,
            last_error: None,
        }
    }

    fn detect_topology(&mut self) {
        match topology::detect() {
            Ok(topo) => {
                info!(
                    arch = %topo.architecture,
                    method = %topo.detect_method,
                    cores = topo.total_cores,
                    ccds = topo.core_groups.len(),
                    "CPU topology detected"
                );
                self.topology = Some(topo);
            }
            Err(e) => {
                warn!("CPU topology detection failed: {}", e);
                self.topology = None;
            }
        }
    }

    fn auto_apply_all(&mut self) -> anyhow::Result<()> {
        let topo = match &self.topology {
            Some(t) => t.clone(),
            None => {
                anyhow::bail!("no topology available");
            }
        };

        let vms = pve::list_vms()?;
        // Collect (vmid, name, cores) for every VM; plan_balanced filters out
        // excluded / 0-core VMs and reserves host cores, identically to the TUI
        // preview so the two never disagree.
        let mut vm_cores: Vec<(u32, String, usize)> = Vec::new();
        for vm in &vms {
            let cfg = pve::read_vm_config(vm.vmid).unwrap_or_default();
            vm_cores.push((vm.vmid, vm.name.clone(), cfg.cores));
        }

        let plan = affinity::plan_balanced(
            &topo,
            &vm_cores,
            self.config.include_smt,
            self.config.reserve_cores,
            &self.config.auto_apply.exclude_vmids,
        );

        let total = plan.len();
        let mut applied = 0;
        for (vmid, name, option) in &plan {
            match pve::set_affinity(*vmid, &option.affinity_str, false) {
                Ok(()) => {
                    info!(vmid, name, affinity = %option.affinity_str, "applied CPU affinity");
                    applied += 1;
                }
                Err(e) => {
                    error!(vmid, name, "failed to apply affinity: {}", e);
                }
            }
        }

        self.applied_count = applied;
        info!(applied, total, "auto-apply complete");
        Ok(())
    }
}

#[async_trait]
impl perigee_daemon::module::Module for AffinityModule {
    fn name(&self) -> &str {
        "affinity"
    }

    async fn init(&mut self, config: &toml::Value) -> anyhow::Result<()> {
        if let Some(aff_val) = config.get("affinity") {
            let aff_str = toml::to_string(aff_val)?;
            let parsed: AffinityFileConfig =
                toml::from_str(&format!("[affinity]\n{}", aff_str)).unwrap_or_default();
            self.config = parsed.affinity;
        } else {
            let path = crate::config::affinity_config_path();
            if path.exists() {
                if let Ok(cfg) = AffinityFileConfig::load(&path) {
                    self.config = cfg.affinity;
                }
            }
        }

        self.detect_topology();
        Ok(())
    }

    async fn apply(&mut self) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        if !self.config.auto_apply.enabled {
            return Ok(());
        }

        if self.topology.is_none() {
            self.detect_topology();
        }

        match self.auto_apply_all() {
            Ok(()) => {
                self.last_error = None;
            }
            Err(e) => {
                let msg = e.to_string();
                error!("affinity auto-apply failed: {}", msg);
                self.last_error = Some(msg);
            }
        }
        Ok(())
    }

    async fn reload(&mut self, config: &toml::Value) -> anyhow::Result<()> {
        if let Some(aff_val) = config.get("affinity") {
            let aff_str = toml::to_string(aff_val)?;
            let parsed: AffinityFileConfig =
                toml::from_str(&format!("[affinity]\n{}", aff_str)).unwrap_or_default();
            self.config = parsed.affinity;
        } else {
            let path = crate::config::affinity_config_path();
            if path.exists() {
                if let Ok(cfg) = AffinityFileConfig::load(&path) {
                    self.config = cfg.affinity;
                }
            }
        }
        self.detect_topology();
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn status(&self) -> ModuleStatus {
        let state = if !self.config.enabled {
            ModuleState::Stopped
        } else if self.last_error.is_some() {
            ModuleState::Error
        } else {
            ModuleState::Running
        };

        ModuleStatus {
            name: "affinity".to_string(),
            state,
            profiles: Vec::new(),
        }
    }
}
