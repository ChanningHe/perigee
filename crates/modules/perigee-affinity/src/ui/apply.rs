use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::{AffinityScreen, AffinityState, AffinityUiAction};
use crate::affinity::cpus_to_ccd_names;
use crate::pve;

// ── Single VM Apply ──

pub fn render_apply(frame: &mut Frame, daemon_online: bool, state: &AffinityState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Min(0),
            Constraint::Length(4),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "CPU Affinity › Apply", daemon_online);

    // Plan summary
    let opt = state.selected_option.as_ref();
    let mut plan_lines = vec![Line::from("")];
    if let Some(opt) = opt {
        let smt_hint =
            if state.include_smt && state.topology.as_ref().map(|t| t.has_smt).unwrap_or(false) {
                format!(" ({} vCPUs with SMT)", state.cores_needed * 2)
            } else {
                String::new()
            };
        plan_lines.push(Line::from(vec![
            Span::styled("  Strategy:   ", common::style_label()),
            Span::styled(opt.name.clone(), common::style_value()),
        ]));
        plan_lines.push(Line::from(vec![
            Span::styled("  Cores:      ", common::style_label()),
            Span::styled(
                format!("{} physical{}", state.cores_needed, smt_hint),
                common::style_value(),
            ),
        ]));
        plan_lines.push(Line::from(vec![
            Span::styled("  Affinity:   ", common::style_label()),
            Span::styled(&opt.affinity_str, Style::default().fg(common::BRAND)),
        ]));
        plan_lines.push(Line::from(vec![
            Span::styled("  Uses:       ", common::style_label()),
            Span::styled(opt.ccds_used.join(", "), common::style_value()),
        ]));
    }
    let plan_block = Paragraph::new(plan_lines).block(
        Block::default()
            .title(Span::styled(
                " Plan ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(plan_block, chunks[1]);

    // VM list
    let header = Line::from(Span::styled(
        format!(
            "  {:<6} {:<16} {:<10} {}",
            "VMID", "Name", "Status", "Current Affinity"
        ),
        common::style_muted(),
    ));

    let items: Vec<ListItem> = state
        .vms
        .iter()
        .enumerate()
        .map(|(i, vm)| {
            let selected = state.vm_list_state.selected() == Some(i);
            let prefix = if selected { " ▸ " } else { "   " };
            let cfg = state.vm_configs.get(&vm.vmid);
            let cur_aff = cfg
                .and_then(|c| c.affinity.as_ref())
                .cloned()
                .unwrap_or_else(|| "—".to_string());

            let style = if selected {
                common::style_selected()
            } else {
                Style::default().fg(common::TEXT_DIM)
            };

            let name_display = if vm.name.len() > 14 {
                format!("{:.14}…", vm.name)
            } else {
                vm.name.clone()
            };

            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(format!("{:<6}", vm.vmid), style),
                Span::styled(format!("{:<16}", name_display), style),
                Span::styled(
                    format!("{:<10}", vm.status),
                    if selected {
                        Style::default().fg(common::TEXT)
                    } else {
                        common::style_muted()
                    },
                ),
                Span::styled(
                    cur_aff,
                    if selected {
                        Style::default().fg(common::TEXT)
                    } else {
                        common::style_muted()
                    },
                ),
            ]))
        })
        .collect();

    let vm_block = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(chunks[2]);

    frame.render_widget(Paragraph::new(header), vm_block[0]);
    let list = List::new(items).block(
        Block::default()
            .title(Span::styled(
                " Target VM ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::TOP)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(list, vm_block[1]);

    // Preview / result
    let mut preview_lines = Vec::new();
    if let (Some(opt), Some(idx)) = (opt, state.vm_list_state.selected()) {
        if let Some(vm) = state.vms.get(idx) {
            preview_lines.push(Line::from(vec![
                Span::styled("  $ ", common::style_muted()),
                Span::styled(
                    format!("qm set {} --affinity {}", vm.vmid, opt.affinity_str),
                    Style::default().fg(common::TEXT_DIM),
                ),
            ]));
        }
    }
    if let Some(ref result) = state.apply_result {
        match result {
            Ok(()) => {
                preview_lines.push(Line::from(Span::styled(
                    "  ✓ Applied successfully",
                    common::style_success(),
                )));
            }
            Err(e) => {
                preview_lines.push(Line::from(Span::styled(
                    format!("  ✗ {}", e),
                    common::style_error(),
                )));
            }
        }
    }
    let preview = Paragraph::new(preview_lines).block(
        Block::default()
            .title(Span::styled(
                " Preview ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(preview, chunks[3]);

    common::footer_bar(
        frame,
        chunks[4],
        &[
            ("↑↓", "Navigate"),
            ("Enter", "Apply"),
            ("d", "Dry Run"),
            ("Esc", "Back"),
        ],
    );
}

pub fn handle_apply_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    let len = state.vms.len();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.apply_result = None;
            AffinityUiAction::NavigateTo(AffinityScreen::Strategy)
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if len > 0 {
                let i = state.vm_list_state.selected().unwrap_or(0);
                let new = if i == 0 { len - 1 } else { i - 1 };
                state.vm_list_state.select(Some(new));
                state.apply_result = None;
            }
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if len > 0 {
                let i = state.vm_list_state.selected().unwrap_or(0);
                let new = if i >= len - 1 { 0 } else { i + 1 };
                state.vm_list_state.select(Some(new));
                state.apply_result = None;
            }
            AffinityUiAction::None
        }
        KeyCode::Enter => {
            do_apply(state, false);
            AffinityUiAction::None
        }
        KeyCode::Char('d') => {
            do_apply(state, true);
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

fn do_apply(state: &mut AffinityState, dry_run: bool) {
    let Some(ref opt) = state.selected_option else {
        return;
    };
    let Some(idx) = state.vm_list_state.selected() else {
        return;
    };
    let Some(vm) = state.vms.get(idx) else { return };

    if dry_run {
        state.apply_result = Some(Ok(()));
        state.message = Some(format!(
            "DRY RUN: qm set {} --affinity {}",
            vm.vmid, opt.affinity_str
        ));
        return;
    }

    match pve::set_affinity(vm.vmid, &opt.affinity_str, false) {
        Ok(()) => {
            state.apply_result = Some(Ok(()));
        }
        Err(e) => {
            state.apply_result = Some(Err(e.to_string()));
        }
    }
}

// ── Auto Apply All ──

pub fn render_auto_apply(frame: &mut Frame, daemon_online: bool, state: &AffinityState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "CPU Affinity › Auto Apply", daemon_online);

    // Plan summary
    let plan_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Strategy:        ", common::style_label()),
            Span::styled("Balanced", common::style_value()),
        ]),
        Line::from(vec![
            Span::styled("  VMs:             ", common::style_label()),
            Span::styled(
                format!("{} to bind", state.auto_plan.len()),
                common::style_value(),
            ),
        ]),
    ];
    let plan_block = Paragraph::new(plan_lines).block(
        Block::default()
            .title(Span::styled(
                " Plan ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(plan_block, chunks[1]);

    // Allocation + commands
    let topo = state.topology.as_ref();
    let mut lines: Vec<Line> = Vec::new();

    if state.auto_plan.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No VMs to bind (VMs need cores > 0 in config)",
            common::style_muted(),
        )));
    } else {
        // Table header
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<6} {:<16} {:>5}  {:<24} {}",
                "VMID", "Name", "Cores", "Affinity", "CCD"
            ),
            common::style_muted(),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(70)),
            Style::default().fg(common::BORDER),
        )));

        for (vmid, name, opt) in &state.auto_plan {
            let ccd_str = if let Some(topo) = topo {
                cpus_to_ccd_names(&opt.cpus, &topo.core_groups).join(", ")
            } else {
                opt.ccds_used.join(", ")
            };

            let name_display = if name.len() > 14 {
                format!("{:.14}…", name)
            } else {
                name.clone()
            };

            lines.push(Line::from(vec![
                Span::styled(format!("  {:<6}", vmid), common::style_value()),
                Span::styled(format!("{:<16}", name_display), common::style_value()),
                Span::styled(format!("{:>5}  ", opt.cpus.len()), common::style_value()),
                Span::styled(
                    format!("{:<24} ", opt.affinity_str),
                    Style::default().fg(common::BRAND),
                ),
                Span::styled(ccd_str, Style::default().fg(common::TEXT_DIM)),
            ]));
        }

        lines.push(Line::from(""));

        if state.auto_executed {
            lines.push(Line::from(Span::styled(
                "  ── Results ──",
                Style::default()
                    .fg(common::TEXT)
                    .add_modifier(Modifier::BOLD),
            )));
            for (vmid, result) in &state.auto_results {
                let (icon, msg, style) = match result {
                    Ok(()) => ("✓", "OK".to_string(), common::style_success()),
                    Err(e) => ("✗", e.clone(), common::style_error()),
                };
                let plan_entry = state.auto_plan.iter().find(|(v, _, _)| v == vmid);
                let aff = plan_entry
                    .map(|(_, _, o)| o.affinity_str.as_str())
                    .unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled(format!("  {} ", icon), style),
                    Span::styled(
                        format!("VM {}  qm set {} --affinity {:<24}", vmid, vmid, aff),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(msg, style),
                ]));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  ── Commands ──",
                Style::default()
                    .fg(common::TEXT)
                    .add_modifier(Modifier::BOLD),
            )));
            for (vmid, _, opt) in &state.auto_plan {
                lines.push(Line::from(vec![
                    Span::styled("  $ ", common::style_muted()),
                    Span::styled(
                        format!("qm set {} --affinity {}", vmid, opt.affinity_str),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                ]));
            }
        }
    }

    let content = Paragraph::new(lines).block(
        Block::default()
            .title(Span::styled(
                " Allocation ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(content, chunks[2]);

    let hints = if state.auto_executed {
        vec![("Esc", "Done")]
    } else {
        vec![("Enter", "Apply All"), ("d", "Dry Run"), ("Esc", "Cancel")]
    };
    common::footer_bar(frame, chunks[3], &hints);
}

pub fn handle_auto_apply_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.auto_executed = false;
            state.auto_results.clear();
            AffinityUiAction::NavigateTo(AffinityScreen::Topology)
        }
        KeyCode::Enter if !state.auto_executed => {
            execute_auto_apply(state, false);
            AffinityUiAction::None
        }
        KeyCode::Char('d') if !state.auto_executed => {
            execute_auto_apply(state, true);
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

fn execute_auto_apply(state: &mut AffinityState, dry_run: bool) {
    state.auto_results.clear();
    for (vmid, _, opt) in &state.auto_plan {
        let result = pve::set_affinity(*vmid, &opt.affinity_str, dry_run);
        state
            .auto_results
            .push((*vmid, result.map_err(|e| e.to_string())));
    }
    state.auto_executed = true;
}
