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
use crate::affinity::Strategy;

pub fn render(frame: &mut Frame, daemon_online: bool, state: &AffinityState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "CPU Affinity › Configure", daemon_online);

    // Parameters section
    let smt_label = if state.include_smt {
        "✓ Yes"
    } else {
        "✗ No"
    };
    let vcpu_hint =
        if state.include_smt && state.topology.as_ref().map(|t| t.has_smt).unwrap_or(false) {
            format!("  ({} vCPUs)", state.cores_needed * 2)
        } else {
            String::new()
        };

    let cores_display = if state.editing_cores {
        format!("[{}▎]", state.cores_input)
    } else {
        format!("[{}]", state.cores_needed)
    };
    let cores_style = if state.editing_cores {
        common::style_editing()
    } else {
        common::style_value()
    };

    let param_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Cores needed:  ", common::style_label()),
            Span::styled(cores_display, cores_style),
        ]),
        Line::from(vec![
            Span::styled("  Include SMT:   ", common::style_label()),
            Span::styled(format!("{}{}", smt_label, vcpu_hint), common::style_value()),
        ]),
    ];
    let param_block = Paragraph::new(param_lines).block(
        Block::default()
            .title(Span::styled(
                " Parameters ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(param_block, chunks[1]);

    // Strategies section
    if state.manual_mode {
        render_manual_select(frame, state, chunks[2]);
    } else {
        render_strategy_list(frame, state, chunks[2]);
    }

    let hints: Vec<(&str, &str)> = if state.editing_cores {
        vec![("Enter", "Confirm"), ("Esc", "Cancel")]
    } else if state.manual_mode {
        vec![("Space", "Toggle"), ("Enter", "Confirm"), ("Esc", "Cancel")]
    } else {
        vec![
            ("↑↓", "Navigate"),
            ("Enter", "Select"),
            ("Tab", "SMT"),
            ("c", "Cores"),
            ("Esc", "Back"),
        ]
    };
    common::footer_bar(frame, chunks[3], &hints);
}

fn render_strategy_list(frame: &mut Frame, state: &AffinityState, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line> = vec![Line::from("")];

    for (i, opt) in state.strategies.iter().enumerate() {
        let selected = i == state.strategy_cursor;
        let prefix = if selected { " ▸ " } else { "   " };

        let name_style = if !opt.available {
            common::style_muted()
        } else if selected {
            common::style_selected()
        } else {
            Style::default().fg(common::TEXT_DIM)
        };

        // Line 1: name + description
        lines.push(Line::from(vec![
            Span::styled(prefix, name_style),
            Span::styled(
                format!("{:<14}", opt.name),
                name_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                opt.description.clone(),
                if selected && opt.available {
                    Style::default().fg(common::TEXT)
                } else {
                    common::style_muted()
                },
            ),
        ]));

        // Line 2: affinity preview (if available and not manual placeholder)
        if opt.available && opt.strategy != Strategy::Manual && !opt.affinity_str.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("                  "),
                Span::styled(
                    format!("→ {}", opt.affinity_str),
                    if selected {
                        Style::default().fg(common::BRAND)
                    } else {
                        Style::default().fg(common::TEXT_MUTED)
                    },
                ),
            ]));

            // Line 3: CCDs used
            if !opt.ccds_used.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("                    "),
                    Span::styled(
                        format!("Uses {}", opt.ccds_used.join(", ")),
                        Style::default().fg(common::TEXT_MUTED),
                    ),
                ]));
            }
        }

        lines.push(Line::from(""));
    }

    if let Some(ref msg) = state.message {
        lines.push(Line::from(Span::styled(
            format!("  {}", msg),
            common::style_warn(),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .title(Span::styled(
                " Strategies ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

fn render_manual_select(frame: &mut Frame, state: &AffinityState, area: ratatui::layout::Rect) {
    let Some(ref topo) = state.topology else {
        return;
    };

    let cores_per_ccd = topo
        .core_groups
        .first()
        .map(|g| g.physical_cpus.len())
        .unwrap_or(8);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  Select CCDs (need {} cores, {} cores/CCD)",
            state.cores_needed, cores_per_ccd
        ),
        Style::default().fg(common::TEXT),
    )));
    lines.push(Line::from(Span::styled(
        format!("  {}", "─".repeat(50)),
        Style::default().fg(common::BORDER),
    )));

    let bindings = state.existing_bindings();
    for (idx, cg) in topo.core_groups.iter().enumerate() {
        let checked = state.ccd_selected.get(idx).copied().unwrap_or(false);
        let check = if checked { "✓" } else { " " };

        let vm_count = bindings
            .iter()
            .filter(|b| b.cpus.iter().any(|c| cg.all_cpus.contains(c)))
            .count();
        let vm_label = if vm_count == 0 {
            "idle".to_string()
        } else {
            format!("{} VMs", vm_count)
        };

        let selected = idx == state.strategy_cursor;
        let style = if selected {
            common::style_selected()
        } else {
            Style::default().fg(common::TEXT_DIM)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  [{}] ", check),
                if checked {
                    common::style_success()
                } else {
                    common::style_muted()
                },
            ),
            Span::styled(
                format!(
                    "{:<8}  {}C    {:<20} {}",
                    cg.name,
                    cg.physical_cpus.len(),
                    crate::affinity::format_cpus(&cg.physical_cpus),
                    vm_label,
                ),
                style,
            ),
        ]));
    }

    let selected_count: usize = state.ccd_selected.iter().filter(|&&s| s).count();
    let selected_cores: usize = state
        .ccd_selected
        .iter()
        .enumerate()
        .filter(|(_, &s)| s)
        .map(|(i, _)| {
            topo.core_groups
                .get(i)
                .map(|g| g.physical_cpus.len())
                .unwrap_or(0)
        })
        .sum();

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  Selected: {} cores from {} CCDs",
            selected_cores, selected_count
        ),
        common::style_value(),
    )));

    let para = Paragraph::new(lines).block(
        Block::default()
            .title(Span::styled(
                " Manual CCD Selection ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

pub fn handle_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    if state.editing_cores {
        return handle_cores_input(state, key);
    }
    if state.manual_mode {
        return handle_manual_input(state, key);
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => AffinityUiAction::NavigateTo(AffinityScreen::Topology),
        KeyCode::Up | KeyCode::Char('k') => {
            if !state.strategies.is_empty() {
                if state.strategy_cursor == 0 {
                    state.strategy_cursor = state.strategies.len() - 1;
                } else {
                    state.strategy_cursor -= 1;
                }
            }
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !state.strategies.is_empty() {
                state.strategy_cursor = (state.strategy_cursor + 1) % state.strategies.len();
            }
            AffinityUiAction::None
        }
        KeyCode::Enter => {
            if let Some(opt) = state.strategies.get(state.strategy_cursor) {
                if !opt.available && opt.strategy != Strategy::Manual {
                    state.message = Some("Strategy not available for this core count".to_string());
                    return AffinityUiAction::None;
                }
                if opt.strategy == Strategy::Manual {
                    state.manual_mode = true;
                    state.strategy_cursor = 0;
                    return AffinityUiAction::None;
                }
                state.selected_option = Some(opt.clone());
                state.apply_result = None;
                state.vm_list_state.select(Some(0));
                AffinityUiAction::NavigateTo(AffinityScreen::Apply)
            } else {
                AffinityUiAction::None
            }
        }
        KeyCode::Tab => {
            state.include_smt = !state.include_smt;
            state.regenerate_strategies();
            AffinityUiAction::None
        }
        KeyCode::Char('c') => {
            state.editing_cores = true;
            state.cores_input = state.cores_needed.to_string();
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

fn handle_cores_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Enter => {
            if let Ok(n) = state.cores_input.parse::<usize>() {
                if n > 0 {
                    state.cores_needed = n;
                    state.regenerate_strategies();
                }
            }
            state.editing_cores = false;
            AffinityUiAction::None
        }
        KeyCode::Esc => {
            state.editing_cores = false;
            AffinityUiAction::None
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            state.cores_input.push(c);
            AffinityUiAction::None
        }
        KeyCode::Backspace => {
            state.cores_input.pop();
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

fn handle_manual_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    let ccd_count = state
        .topology
        .as_ref()
        .map(|t| t.core_groups.len())
        .unwrap_or(0);

    match key.code {
        KeyCode::Esc => {
            state.manual_mode = false;
            state.strategy_cursor = state
                .strategies
                .iter()
                .position(|o| o.strategy == Strategy::Manual)
                .unwrap_or(0);
            AffinityUiAction::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if ccd_count > 0 {
                if state.strategy_cursor == 0 {
                    state.strategy_cursor = ccd_count - 1;
                } else {
                    state.strategy_cursor -= 1;
                }
            }
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if ccd_count > 0 {
                state.strategy_cursor = (state.strategy_cursor + 1) % ccd_count;
            }
            AffinityUiAction::None
        }
        KeyCode::Char(' ') => {
            if let Some(sel) = state.ccd_selected.get_mut(state.strategy_cursor) {
                *sel = !*sel;
            }
            AffinityUiAction::None
        }
        KeyCode::Enter => {
            let selected_indices: Vec<usize> = state
                .ccd_selected
                .iter()
                .enumerate()
                .filter(|(_, &s)| s)
                .map(|(i, _)| i)
                .collect();

            if selected_indices.is_empty() {
                state.message = Some("No CCDs selected".to_string());
                return AffinityUiAction::None;
            }

            let Some(ref topo) = state.topology else {
                return AffinityUiAction::None;
            };

            let req = crate::affinity::AffinityRequest {
                cores_needed: state.cores_needed,
                include_smt: state.include_smt,
                topology: topo.clone(),
                existing_bindings: state.existing_bindings(),
            };

            match crate::affinity::generate_manual(&req, &selected_indices) {
                Ok(opt) => {
                    state.selected_option = Some(opt);
                    state.manual_mode = false;
                    state.apply_result = None;
                    state.vm_list_state.select(Some(0));
                    AffinityUiAction::NavigateTo(AffinityScreen::Apply)
                }
                Err(e) => {
                    state.message = Some(format!("Error: {}", e));
                    AffinityUiAction::None
                }
            }
        }
        _ => AffinityUiAction::None,
    }
}
