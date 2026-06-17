pub mod apply;
pub mod strategy;
pub mod topology_view;

use crossterm::event::KeyEvent;
use ratatui::widgets::ListState;
use std::collections::HashMap;
use std::sync::mpsc;

use crate::affinity::{self, AffinityOption, AffinityRequest, VmBinding};
use crate::pve;
use crate::topology::CpuTopology;

type TopoResult = Result<CpuTopology, String>;
type VmsResult = (Vec<pve::Vm>, HashMap<u32, pve::VmConfig>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityScreen {
    Topology,
    Strategy,
    Apply,
    AutoApply,
}

#[derive(Debug)]
pub enum AffinityUiAction {
    None,
    NavigateTo(AffinityScreen),
    GoBack,
}

pub struct AffinityState {
    pub topology: Option<CpuTopology>,
    pub topo_error: Option<String>,

    pub vms: Vec<pve::Vm>,
    pub vm_configs: HashMap<u32, pve::VmConfig>,
    pub vm_list_state: ListState,

    pub cores_input: String,
    pub cores_needed: usize,
    pub include_smt: bool,
    pub editing_cores: bool,

    pub strategies: Vec<AffinityOption>,
    pub strategy_cursor: usize,

    pub manual_mode: bool,
    pub ccd_selected: Vec<bool>,

    pub selected_option: Option<AffinityOption>,

    // Auto-apply config mirrored from /etc/perigee/affinity.toml so the preview
    // matches what the daemon would actually apply.
    pub reserve_cores: usize,
    pub exclude_vmids: Vec<u32>,

    pub auto_plan: Vec<(u32, String, AffinityOption)>,
    pub auto_results: Vec<(u32, Result<(), String>)>,
    pub auto_executed: bool,
    /// Scroll offset for the auto-apply allocation/results view, which grows
    /// with the VM count.
    pub auto_scroll: u16,
    pub apply_result: Option<Result<(), String>>,

    pub message: Option<String>,

    pub topo_scroll: usize,
    pub topo_max_scroll: usize,

    topo_rx: Option<mpsc::Receiver<TopoResult>>,
    vms_rx: Option<mpsc::Receiver<VmsResult>>,
    pub data_ready: bool,
}

impl Default for AffinityState {
    fn default() -> Self {
        Self::new()
    }
}

impl AffinityState {
    pub fn new() -> Self {
        // Mirror the daemon's config so the auto-apply preview agrees with it.
        // Missing/unreadable config falls back to defaults.
        let cfg = crate::config::AffinityFileConfig::load(&crate::config::affinity_config_path())
            .map(|f| f.affinity)
            .unwrap_or_default();
        Self {
            topology: None,
            topo_error: None,
            vms: Vec::new(),
            vm_configs: HashMap::new(),
            vm_list_state: ListState::default(),
            cores_input: "4".to_string(),
            cores_needed: 4,
            include_smt: cfg.include_smt,
            editing_cores: false,
            strategies: Vec::new(),
            strategy_cursor: 0,
            manual_mode: false,
            ccd_selected: Vec::new(),
            selected_option: None,
            reserve_cores: cfg.reserve_cores,
            exclude_vmids: cfg.auto_apply.exclude_vmids,
            auto_plan: Vec::new(),
            auto_results: Vec::new(),
            auto_executed: false,
            auto_scroll: 0,
            apply_result: None,
            message: None,
            topo_scroll: 0,
            topo_max_scroll: 0,
            topo_rx: None,
            vms_rx: None,
            data_ready: false,
        }
    }

    /// Kick off background loading of topology + VMs (non-blocking).
    pub fn preload(&mut self) {
        if self.topo_rx.is_some() || self.topology.is_some() {
            return;
        }
        let (topo_tx, topo_rx) = mpsc::channel();
        let (vms_tx, vms_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = crate::topology::detect().map_err(|e| e.to_string());
            let _ = topo_tx.send(result);
        });
        std::thread::spawn(move || {
            let vms = pve::list_vms().unwrap_or_default();
            let mut configs = HashMap::new();
            for vm in &vms {
                if let Ok(cfg) = pve::read_vm_config(vm.vmid) {
                    configs.insert(vm.vmid, cfg);
                }
            }
            let _ = vms_tx.send((vms, configs));
        });
        self.topo_rx = Some(topo_rx);
        self.vms_rx = Some(vms_rx);
    }

    /// Poll background channels; returns true if new data arrived.
    pub fn poll_preload(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = self.topo_rx.take() {
            match rx.try_recv() {
                Ok(Ok(topo)) => {
                    let n = topo.core_groups.len();
                    self.topology = Some(topo);
                    self.topo_error = None;
                    self.ccd_selected = vec![false; n];
                    changed = true;
                }
                Ok(Err(e)) => {
                    self.topo_error = Some(e);
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.topo_rx = Some(rx);
                }
                Err(_) => {}
            }
        }
        if let Some(rx) = self.vms_rx.take() {
            match rx.try_recv() {
                Ok((vms, configs)) => {
                    self.vms = vms;
                    self.vm_configs = configs;
                    if !self.vms.is_empty() && self.vm_list_state.selected().is_none() {
                        self.vm_list_state.select(Some(0));
                    }
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.vms_rx = Some(rx);
                }
                Err(_) => {}
            }
        }
        if changed && self.topo_rx.is_none() && self.vms_rx.is_none() {
            self.data_ready = true;
        }
        changed
    }

    pub fn detect_topology(&mut self) {
        match crate::topology::detect() {
            Ok(topo) => {
                let n = topo.core_groups.len();
                self.topology = Some(topo);
                self.topo_error = None;
                self.ccd_selected = vec![false; n];
            }
            Err(e) => {
                self.topo_error = Some(e.to_string());
                self.topology = None;
            }
        }
    }

    pub fn refresh_vms(&mut self) {
        match pve::list_vms() {
            Ok(vms) => {
                self.vm_configs.clear();
                for vm in &vms {
                    if let Ok(cfg) = pve::read_vm_config(vm.vmid) {
                        self.vm_configs.insert(vm.vmid, cfg);
                    }
                }
                self.vms = vms;
                if !self.vms.is_empty() && self.vm_list_state.selected().is_none() {
                    self.vm_list_state.select(Some(0));
                }
            }
            Err(_) => {
                self.vms.clear();
                self.vm_configs.clear();
            }
        }
    }

    pub fn existing_bindings(&self) -> Vec<VmBinding> {
        let mut bindings = Vec::new();
        for (vmid, cfg) in &self.vm_configs {
            if let Some(ref aff) = cfg.affinity {
                let cpus = affinity::parse_affinity_str(aff);
                if !cpus.is_empty() {
                    bindings.push(VmBinding { vmid: *vmid, cpus });
                }
            }
        }
        bindings
    }

    pub fn regenerate_strategies(&mut self) {
        let Some(topo) = &self.topology else { return };
        let req = AffinityRequest {
            cores_needed: self.cores_needed,
            include_smt: self.include_smt,
            topology: topo.clone(),
            existing_bindings: self.existing_bindings(),
        };
        match affinity::generate(&req) {
            Ok(opts) => {
                self.strategies = opts;
                if self.strategy_cursor >= self.strategies.len() {
                    self.strategy_cursor = 0;
                }
            }
            Err(e) => {
                self.message = Some(format!("Generate error: {}", e));
                self.strategies.clear();
            }
        }
    }

    pub fn generate_auto_plan(&mut self) {
        let Some(topo) = &self.topology else { return };

        self.auto_results.clear();
        self.auto_executed = false;
        self.auto_scroll = 0;

        let vm_cores: Vec<(u32, String, usize)> = self
            .vms
            .iter()
            .map(|vm| {
                let cores = self.vm_configs.get(&vm.vmid).map(|c| c.cores).unwrap_or(0);
                (vm.vmid, vm.name.clone(), cores)
            })
            .collect();

        // Same planner the daemon uses, so the preview reflects reserved cores
        // and the exclude list rather than a different allocation.
        let plan = affinity::plan_balanced(
            topo,
            &vm_cores,
            self.include_smt,
            self.reserve_cores,
            &self.exclude_vmids,
        );
        self.auto_plan = plan;
    }
}

// ── Render / input dispatch (called from perigee-cli) ──

pub fn render_topology(frame: &mut ratatui::Frame, daemon_online: bool, state: &mut AffinityState) {
    topology_view::render(frame, daemon_online, state);
}

pub fn handle_topology_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    topology_view::handle_input(state, key)
}

pub fn render_strategy(frame: &mut ratatui::Frame, daemon_online: bool, state: &AffinityState) {
    strategy::render(frame, daemon_online, state);
}

pub fn handle_strategy_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    strategy::handle_input(state, key)
}

pub fn render_apply(frame: &mut ratatui::Frame, daemon_online: bool, state: &mut AffinityState) {
    apply::render_apply(frame, daemon_online, state);
}

pub fn handle_apply_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    apply::handle_apply_input(state, key)
}

pub fn render_auto_apply(
    frame: &mut ratatui::Frame,
    daemon_online: bool,
    state: &mut AffinityState,
) {
    apply::render_auto_apply(frame, daemon_online, state);
}

pub fn handle_auto_apply_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    apply::handle_auto_apply_input(state, key)
}
