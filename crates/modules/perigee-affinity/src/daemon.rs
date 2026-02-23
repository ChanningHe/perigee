use async_trait::async_trait;
use perigee_core::ipc::{ModuleState, ModuleStatus};
use tracing::{error, info, warn};

use crate::affinity::{self, AffinityRequest, Strategy, VmBinding};
use crate::config::{AffinityConfig, AffinityFileConfig};
use crate::pve;
use crate::topology::{self, CpuTopology};

pub struct AffinityModule {
    config: AffinityConfig,
    topology: Option<CpuTopology>,
    applied_count: usize,
    last_error: Option<String>,
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
        let exclude = &self.config.auto_apply.exclude_vmids;

        // Gather VM configs and filter
        let mut vm_entries: Vec<(u32, String, usize)> = Vec::new();
        for vm in &vms {
            if exclude.contains(&vm.vmid) {
                continue;
            }
            let cfg = pve::read_vm_config(vm.vmid).unwrap_or_default();
            if cfg.cores == 0 {
                continue;
            }
            vm_entries.push((vm.vmid, vm.name.clone(), cfg.cores));
        }

        // Sort by cores descending (allocate large VMs first)
        vm_entries.sort_by(|a, b| b.2.cmp(&a.2));

        // Build reserved cores set
        let reserve = self.config.reserve_cores;
        let mut reserved_cpus: Vec<usize> = Vec::new();
        if reserve > 0 {
            let mut count = 0;
            for cg in &topo.core_groups {
                for &cpu in &cg.physical_cpus {
                    if count >= reserve {
                        break;
                    }
                    reserved_cpus.push(cpu);
                    count += 1;
                }
                if count >= reserve {
                    break;
                }
            }
        }

        // Iteratively assign using balanced strategy
        let mut current_bindings: Vec<VmBinding> = Vec::new();

        // Include reserved cores as a pseudo-binding
        if !reserved_cpus.is_empty() {
            current_bindings.push(VmBinding {
                vmid: 0,
                cpus: reserved_cpus,
            });
        }

        let mut applied = 0;
        for (vmid, name, cores) in &vm_entries {
            let req = AffinityRequest {
                cores_needed: *cores,
                include_smt: self.config.include_smt,
                topology: topo.clone(),
                existing_bindings: current_bindings.clone(),
            };

            let options = match affinity::generate(&req) {
                Ok(opts) => opts,
                Err(e) => {
                    warn!(vmid, name, "failed to generate affinity: {}", e);
                    continue;
                }
            };

            // Pick balanced strategy
            let option = options
                .iter()
                .find(|o| o.strategy == Strategy::Balanced && o.available)
                .or_else(|| options.iter().find(|o| o.available));

            let Some(option) = option else {
                warn!(vmid, name, "no available strategy");
                continue;
            };

            match pve::set_affinity(*vmid, &option.affinity_str, false) {
                Ok(()) => {
                    info!(vmid, name, affinity = %option.affinity_str, "applied CPU affinity");
                    current_bindings.push(VmBinding {
                        vmid: *vmid,
                        cpus: option.cpus.clone(),
                    });
                    applied += 1;
                }
                Err(e) => {
                    error!(vmid, name, "failed to apply affinity: {}", e);
                }
            }
        }

        self.applied_count = applied;
        info!(applied, total = vm_entries.len(), "auto-apply complete");
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
            let parsed: AffinityFileConfig = toml::from_str(&format!("[affinity]\n{}", aff_str))
                .unwrap_or_default();
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
            let parsed: AffinityFileConfig = toml::from_str(&format!("[affinity]\n{}", aff_str))
                .unwrap_or_default();
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
