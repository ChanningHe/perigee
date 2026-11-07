//! TUI for the normalized affinity planner.
//!
//! The UI displays the same topology, inventory and plan types used by the
//! daemon. It contains no allocation algorithm of its own.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::sync::mpsc;

use crate::config::AffinityFileConfig;
use crate::inventory::{self, InventorySnapshot};
use crate::model::{AffinityPlan, CpuTopology, ReconcileOutcome};
use crate::planner;
use crate::reconcile::{self, PveBackend};
use crate::topology_adapter;

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

type LoadResult = Result<(CpuTopology, InventorySnapshot), String>;

pub struct AffinityState {
    pub topology: Option<CpuTopology>,
    pub inventory: Option<InventorySnapshot>,
    pub plan: Option<AffinityPlan>,
    pub error: Option<String>,
    pub message: Option<String>,
    pub scroll: u16,
    pub loaded: bool,
    rx: Option<mpsc::Receiver<LoadResult>>,
}

impl Default for AffinityState {
    fn default() -> Self {
        Self::new()
    }
}

impl AffinityState {
    pub fn new() -> Self {
        Self {
            topology: None,
            inventory: None,
            plan: None,
            error: None,
            message: None,
            scroll: 0,
            loaded: false,
            rx: None,
        }
    }

    pub fn preload(&mut self) {
        if self.rx.is_some() || self.loaded {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = topology_adapter::detect()
                .and_then(|topology| inventory::collect(&[]).map(|inventory| (topology, inventory)))
                .map_err(|error| format!("loading affinity state: {error:#}"));
            let _ = tx.send(result);
        });
        self.rx = Some(rx);
    }

    pub fn poll_preload(&mut self) -> bool {
        let Some(rx) = self.rx.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok((topology, inventory))) => {
                self.topology = Some(topology);
                self.inventory = Some(inventory);
                self.loaded = true;
                self.error = None;
                self.generate_plan();
                true
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                true
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.rx = Some(rx);
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("affinity loader disconnected".to_string());
                true
            }
        }
    }

    pub fn refresh(&mut self) {
        self.loaded = false;
        self.topology = None;
        self.inventory = None;
        self.plan = None;
        self.error = None;
        self.preload();
    }

    pub fn generate_plan(&mut self) {
        let (Some(topology), Some(inventory)) = (&self.topology, &self.inventory) else {
            return;
        };
        let config = match AffinityFileConfig::load(&crate::config::affinity_config_path()) {
            Ok(config) => config.affinity,
            Err(_) => Default::default(),
        };
        self.plan = Some(planner::plan(topology, &inventory.vms, &config.policy()));
    }

    fn apply_plan(&mut self, dry_run: bool) {
        let Some(plan) = &self.plan else {
            self.message = Some("No affinity plan is available".to_string());
            return;
        };
        if !plan.conflicts.is_empty() {
            self.message = Some(format!("Plan has {} conflict(s)", plan.conflicts.len()));
            return;
        }
        if dry_run {
            self.message = Some(format!(
                "DRY RUN: {} VM(s) would change",
                plan.assignments
                    .iter()
                    .filter(|assignment| assignment.changed)
                    .count()
            ));
            return;
        }

        let config = match AffinityFileConfig::load(&crate::config::affinity_config_path()) {
            Ok(config) => config.affinity,
            Err(_) => Default::default(),
        };
        match reconcile::reconcile(
            &PveBackend,
            &config.policy(),
            &config.auto_apply.exclude_vmids,
        ) {
            Ok(report) => {
                self.message = Some(format_report(
                    &report.outcome,
                    report.changed,
                    report.failed,
                ));
                self.generate_plan();
            }
            Err(error) => self.message = Some(format!("Apply failed: {error:#}")),
        }
    }
}

fn format_report(outcome: &ReconcileOutcome, changed: usize, failed: usize) -> String {
    format!("Reconcile {outcome:?}: {changed} changed, {failed} failed")
}

pub fn render_topology(frame: &mut Frame, daemon_online: bool, state: &mut AffinityState) {
    render(frame, daemon_online, state, "CPU Affinity");
}

pub fn render_strategy(frame: &mut Frame, daemon_online: bool, state: &AffinityState) {
    render_plan(frame, daemon_online, state, "CPU Affinity › Plan");
}

pub fn render_apply(frame: &mut Frame, daemon_online: bool, state: &mut AffinityState) {
    render_plan(frame, daemon_online, state, "CPU Affinity › Apply");
}

pub fn render_auto_apply(frame: &mut Frame, daemon_online: bool, state: &mut AffinityState) {
    render_plan(frame, daemon_online, state, "CPU Affinity › Auto Apply");
}

fn render(frame: &mut Frame, daemon_online: bool, state: &mut AffinityState, title: &str) {
    let lines = topology_lines(state);
    render_lines(
        frame,
        daemon_online,
        &mut AffinityStateView { state },
        title,
        lines,
        &[
            ("Enter", "Plan"),
            ("a", "Auto"),
            ("r", "Refresh"),
            ("q", "Back"),
        ],
    );
}

fn render_plan(frame: &mut Frame, daemon_online: bool, state: &AffinityState, title: &str) {
    let lines = plan_lines(state);
    render_lines(
        frame,
        daemon_online,
        &mut AffinityStateView { state },
        title,
        lines,
        &[("Enter", "Apply"), ("d", "Dry Run"), ("q", "Back")],
    );
}

struct AffinityStateView<'a> {
    state: &'a AffinityState,
}

fn render_lines(
    frame: &mut Frame,
    daemon_online: bool,
    state: &mut AffinityStateView<'_>,
    title: &str,
    mut lines: Vec<Line<'static>>,
    hints: &[(&str, &str)],
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());
    perigee_tui::header_bar(frame, chunks[0], title, daemon_online);
    if let Some(message) = &state.state.message {
        lines.push(Line::from(Span::styled(
            message.clone(),
            perigee_tui::style_warn(),
        )));
    }
    let viewport = chunks[1].height.saturating_sub(2);
    let scroll = state
        .state
        .scroll
        .min((lines.len() as u16).saturating_sub(viewport));
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(perigee_tui::BORDER)),
        ),
        chunks[1],
    );
    perigee_tui::footer_bar(frame, chunks[2], hints);
}

fn topology_lines(state: &AffinityState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(error) = &state.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            perigee_tui::style_error(),
        )));
        return lines;
    }
    let Some(topology) = &state.topology else {
        lines.push(Line::from("Loading topology and VM inventory..."));
        return lines;
    };
    lines.push(Line::from(Span::styled(
        "── Topology ──",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!("Quality: {:?}", topology.quality)));
    lines.push(Line::from(format!(
        "Cache domain source: {:?}",
        topology.cache_domain_source
    )));
    lines.push(Line::from(format!(
        "Logical CPUs: {}",
        topology.logical_cpus.len()
    )));
    lines.push(Line::from(format!(
        "Physical cores: {}",
        topology.physical_cores.len()
    )));
    lines.push(Line::from(""));
    if let Some(inventory) = &state.inventory {
        lines.push(Line::from(Span::styled(
            "── VM Inventory ──",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for vm in &inventory.vms {
            lines.push(Line::from(format!(
                "VM {:>5}  {:<20} {:>3} active / {:>3} max  {:?}  {:?}",
                vm.vmid,
                vm.name,
                vm.cpu.active_vcpus,
                vm.cpu.max_vcpus,
                vm.ownership,
                vm.current_affinity
            )));
        }
        for warning in &inventory.warnings {
            lines.push(Line::from(Span::styled(
                format!("VM {}: {}", warning.vmid, warning.message),
                perigee_tui::style_warn(),
            )));
        }
    }
    lines
}

fn plan_lines(state: &AffinityState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let Some(plan) = &state.plan else {
        lines.push(Line::from("No plan available"));
        return lines;
    };
    lines.push(Line::from(Span::styled(
        "── Desired Affinity Plan ──",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "Topology quality: {:?}",
        plan.topology_quality
    )));
    for assignment in &plan.assignments {
        lines.push(Line::from(format!(
            "VM {:>5}  {:>3} vCPU  {:<24}  {}",
            assignment.vmid,
            assignment.active_vcpus,
            assignment.allowed_cpus,
            if assignment.changed {
                "change"
            } else {
                "unchanged"
            }
        )));
    }
    for warning in &plan.warnings {
        lines.push(Line::from(Span::styled(
            format!("Warning: {}", warning.message),
            perigee_tui::style_warn(),
        )));
    }
    for conflict in &plan.conflicts {
        lines.push(Line::from(Span::styled(
            format!("Conflict: {}", conflict.message),
            perigee_tui::style_error(),
        )));
    }
    lines
}

pub fn handle_topology_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => AffinityUiAction::GoBack,
        KeyCode::Enter => {
            state.generate_plan();
            AffinityUiAction::NavigateTo(AffinityScreen::Strategy)
        }
        KeyCode::Char('a') => {
            state.generate_plan();
            AffinityUiAction::NavigateTo(AffinityScreen::AutoApply)
        }
        KeyCode::Char('r') => {
            state.refresh();
            AffinityUiAction::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.scroll = state.scroll.saturating_sub(1);
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.scroll = state.scroll.saturating_add(1);
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

pub fn handle_strategy_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => AffinityUiAction::NavigateTo(AffinityScreen::Topology),
        KeyCode::Enter => AffinityUiAction::NavigateTo(AffinityScreen::Apply),
        KeyCode::Up | KeyCode::Char('k') => {
            state.scroll = state.scroll.saturating_sub(1);
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.scroll = state.scroll.saturating_add(1);
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

pub fn handle_apply_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => AffinityUiAction::NavigateTo(AffinityScreen::Strategy),
        KeyCode::Enter => {
            state.apply_plan(false);
            AffinityUiAction::None
        }
        KeyCode::Char('d') => {
            state.apply_plan(true);
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

pub fn handle_auto_apply_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => AffinityUiAction::NavigateTo(AffinityScreen::Topology),
        KeyCode::Enter => {
            state.apply_plan(false);
            AffinityUiAction::None
        }
        KeyCode::Char('d') => {
            state.apply_plan(true);
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}
