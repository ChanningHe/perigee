use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::{AffinityScreen, AffinityState, AffinityUiAction};
use crate::affinity::{cpus_to_ccd_names, parse_affinity_str};
use crate::topology::Architecture;

pub fn render(frame: &mut Frame, daemon_online: bool, state: &AffinityState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "CPU Affinity", daemon_online);

    let mut lines: Vec<Line> = Vec::new();

    if let Some(ref err) = state.topo_error {
        lines.push(Line::from(Span::styled(
            format!("  Topology detection failed: {}", err),
            common::style_error(),
        )));
    } else if let Some(ref topo) = state.topology {
        let section = |text: &str| -> Line<'static> {
            Line::from(Span::styled(
                format!("  {}", text),
                Style::default()
                    .fg(common::TEXT)
                    .add_modifier(Modifier::BOLD),
            ))
        };
        let kv = |label: &str, value: String| -> Line<'static> {
            Line::from(vec![
                Span::styled(format!("  {:<16}", label), common::style_label()),
                Span::styled(value, common::style_value()),
            ])
        };

        // CPU info (compact)
        lines.push(section("── CPU Info ──"));
        lines.push(kv("Architecture:", topo.architecture.to_string()));
        lines.push(kv(
            "Total:",
            format!(
                "{} logical / {} physical / SMT {}",
                topo.total_cpus,
                topo.total_cores,
                if topo.has_smt { "✓" } else { "✗" }
            ),
        ));
        lines.push(Line::from(""));

        // CCD / Core Group listing with load bars
        let bindings = state.existing_bindings();
        let mut ccd_thread_count: Vec<usize> = vec![0; topo.core_groups.len()];
        let mut ccd_vm_count: Vec<usize> = vec![0; topo.core_groups.len()];
        for binding in &bindings {
            let mut touched = std::collections::HashSet::new();
            for &cpu in &binding.cpus {
                for (idx, cg) in topo.core_groups.iter().enumerate() {
                    if cg.all_cpus.contains(&cpu) {
                        ccd_thread_count[idx] += 1;
                        touched.insert(idx);
                    }
                }
            }
            for idx in touched {
                ccd_vm_count[idx] += 1;
            }
        }

        let bar_width = 8usize;

        match topo.architecture {
            Architecture::IntelHybrid => {
                lines.push(section("── Core Groups ──"));
                for (idx, cg) in topo.core_groups.iter().enumerate() {
                    let cores = cg.physical_cpus.len();
                    let threads = cg.all_cpus.len();
                    let used = ccd_thread_count[idx].min(threads);
                    let filled = if threads > 0 {
                        (used * bar_width) / threads
                    } else {
                        0
                    };
                    let empty = bar_width - filled;
                    let bar_color = load_bar_color(used, threads);

                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {:<10}", cg.name),
                            common::style_selected(),
                        ),
                        Span::styled(
                            format!("{}C/{}T  ", cores, threads),
                            common::style_value(),
                        ),
                        Span::styled(
                            "█".repeat(filled),
                            Style::default().fg(bar_color),
                        ),
                        Span::styled(
                            "░".repeat(empty),
                            Style::default().fg(common::TEXT_MUTED),
                        ),
                        Span::styled(
                            format!("  {}", load_label(ccd_vm_count[idx], used, threads)),
                            Style::default().fg(common::TEXT_DIM),
                        ),
                    ]));
                }
            }
            _ => {
                for pkg in &topo.packages {
                    lines.push(section(&format!("── Package {} ──", pkg.id)));
                    for cg in &pkg.core_groups {
                        let global_idx = topo
                            .core_groups
                            .iter()
                            .position(|g| g.name == cg.name && g.package_id == cg.package_id)
                            .unwrap_or(0);
                        let cores = cg.physical_cpus.len();
                        let threads = cg.all_cpus.len();
                        let used = ccd_thread_count[global_idx].min(threads);
                        let filled = if threads > 0 {
                            (used * bar_width) / threads
                        } else {
                            0
                        };
                        let empty = bar_width - filled;
                        let bar_color = load_bar_color(used, threads);

                        let l3_label = if cg.l3_cache_id >= 0 {
                            format!("L3#{:<3}", cg.l3_cache_id)
                        } else {
                            "     ".to_string()
                        };

                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {:<7}", cg.name),
                                common::style_selected(),
                            ),
                            Span::styled(
                                format!("{} ", l3_label),
                                Style::default().fg(common::TEXT_MUTED),
                            ),
                            Span::styled(
                                format!("{}C/{}T  ", cores, threads),
                                common::style_value(),
                            ),
                            Span::styled(
                                "█".repeat(filled),
                                Style::default().fg(bar_color),
                            ),
                            Span::styled(
                                "░".repeat(empty),
                                Style::default().fg(common::TEXT_MUTED),
                            ),
                            Span::styled(
                                format!("  {}", load_label(ccd_vm_count[global_idx], used, threads)),
                                Style::default().fg(common::TEXT_DIM),
                            ),
                        ]));
                    }
                    lines.push(Line::from(""));
                }
            }
        }

        // VM Bindings table
        if !state.vms.is_empty() {
            lines.push(section("── VM Bindings ──"));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {:<6} {:<14} {:<9} {:>5}  {:<20} {}",
                    "VMID", "Name", "Status", "Cores", "Affinity", "CCD"
                ),
                common::style_muted(),
            )));

            let mut bound = 0usize;
            for vm in &state.vms {
                let cfg = state.vm_configs.get(&vm.vmid);
                let cores = cfg.map(|c| c.cores).unwrap_or(0);
                let (aff_str, ccd_str) =
                    if let Some(aff) = cfg.and_then(|c| c.affinity.as_ref()) {
                        bound += 1;
                        let cpus = parse_affinity_str(aff);
                        let ccds = cpus_to_ccd_names(&cpus, &topo.core_groups);
                        (aff.to_string(), ccds.join(", "))
                    } else {
                        ("—".to_string(), "—".to_string())
                    };

                let status_color = if vm.status == "running" {
                    common::SUCCESS
                } else {
                    common::TEXT_MUTED
                };

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<6}", vm.vmid), common::style_value()),
                    Span::styled(
                        format!("{:<14}", vm.name),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(
                        format!("{:<9}", vm.status),
                        Style::default().fg(status_color),
                    ),
                    Span::styled(
                        format!("{:>5}  ", cores),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(
                        format!("{:<20} ", aff_str),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(ccd_str, Style::default().fg(common::BRAND_DIM)),
                ]));
            }

            let unbound = state.vms.len() - bound;
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} VMs, {} bound, {} unbound",
                    state.vms.len(),
                    bound,
                    unbound
                ),
                common::style_muted(),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  Loading topology...",
            common::style_muted(),
        )));
    }

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(common::BORDER)),
        )
        .scroll((state.topo_scroll as u16, 0));
    frame.render_widget(para, chunks[1]);

    common::footer_bar(
        frame,
        chunks[2],
        &[
            ("Enter", "Configure"),
            ("a", "Auto Apply"),
            ("r", "Refresh"),
            ("q", "Back"),
        ],
    );
}

fn load_bar_color(used: usize, total: usize) -> ratatui::style::Color {
    if used == 0 {
        common::SUCCESS
    } else if used >= total {
        common::WARN
    } else {
        common::BRAND
    }
}

fn load_label(vm_count: usize, used: usize, total: usize) -> String {
    if used == 0 {
        "idle".to_string()
    } else {
        let vm_word = if vm_count == 1 { "VM" } else { "VMs" };
        format!("{} {}, {}/{}", vm_count, vm_word, used, total)
    }
}

pub fn handle_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => AffinityUiAction::GoBack,
        KeyCode::Enter => {
            state.editing_cores = true;
            state.cores_input = state.cores_needed.to_string();
            state.regenerate_strategies();
            AffinityUiAction::NavigateTo(AffinityScreen::Strategy)
        }
        KeyCode::Char('a') => {
            state.generate_auto_plan();
            AffinityUiAction::NavigateTo(AffinityScreen::AutoApply)
        }
        KeyCode::Char('r') => {
            state.detect_topology();
            state.refresh_vms();
            state.message = Some("Refreshed".to_string());
            AffinityUiAction::None
        }
        KeyCode::Char('t') => {
            if let Some(ref topo) = state.topology {
                if let Ok(json) = serde_json::to_string_pretty(topo) {
                    state.message = Some(format!("Topology JSON:\n{}", json));
                }
            }
            AffinityUiAction::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.topo_scroll > 0 {
                state.topo_scroll -= 1;
            }
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.topo_scroll += 1;
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}
