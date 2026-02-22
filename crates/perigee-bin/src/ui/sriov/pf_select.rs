use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::AppState;

pub fn render(frame: &mut Frame, state: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Profile name input
    let name_block = Paragraph::new(Line::from(vec![
        Span::styled("  Profile Name: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{}]", &state.sriov_state.editing_name),
            Style::default().fg(Color::White),
        ),
    ]))
    .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(name_block, chunks[0]);

    // PF list or error
    let pfs = &state.sriov_state.detected_pfs;
    let selected = state.sriov_state.selected_pf;

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "  Select Physical Function:",
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(""));

    if let Some(err) = &state.sriov_state.pf_scan_error {
        lines.push(Line::from(Span::styled(
            format!("  Scan error: {}", err),
            Style::default().fg(Color::Red),
        )));
    } else if pfs.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No SR-IOV capable PFs detected.",
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(Span::styled(
            "  Check: IOMMU enabled, SR-IOV support in BIOS, driver loaded.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Table header
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<3} {:<14} {:<14} {:<20} {:<10} {:>6} {:>5}",
                "", "Iface", "PCI Address", "MAC", "Vendor", "MaxVF", "CurVF"
            ),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(80)),
            Style::default().fg(Color::DarkGray),
        )));

        for (i, pf) in pfs.iter().enumerate() {
            let is_selected = i == selected;
            let prefix = if is_selected { " ▸ " } else { "   " };
            let style = if is_selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            lines.push(Line::from(Span::styled(
                format!(
                    "{}{:<14} {:<14} {:<20} {:<10} {:>6} {:>5}",
                    prefix,
                    pf.iface_name,
                    pf.pci_address,
                    pf.mac_address,
                    pf.vendor,
                    pf.max_vfs,
                    pf.current_vfs,
                ),
                style,
            )));
        }

        // Selected PF details
        if let Some(pf) = pfs.get(selected) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ── Selected PF ──",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(vec![
                Span::styled("  Driver: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&pf.driver, Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Link:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        "{}{}",
                        pf.link_state,
                        pf.speed
                            .as_ref()
                            .map(|s| format!("  Speed: {}Mb/s", s))
                            .unwrap_or_default()
                    ),
                    Style::default().fg(if pf.link_state == perigee_sriov::detect::LinkState::Up {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
            ]));
        }
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, chunks[1]);
}

pub fn handle_input(state: &mut AppState, key: KeyEvent) {
    let pf_count = state.sriov_state.detected_pfs.len();

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if pf_count > 0 {
                if state.sriov_state.selected_pf == 0 {
                    state.sriov_state.selected_pf = pf_count - 1;
                } else {
                    state.sriov_state.selected_pf -= 1;
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if pf_count > 0 {
                state.sriov_state.selected_pf = (state.sriov_state.selected_pf + 1) % pf_count;
            }
        }
        KeyCode::Enter => {
            if let Some(pf) = state.sriov_state.detected_pfs.get(state.sriov_state.selected_pf) {
                // Initialize or update editing profile with selected PF
                let mac = pf.mac_address;
                if let Some(ref mut profile) = state.sriov_state.editing_profile {
                    profile.mac = mac;
                } else {
                    state.sriov_state.editing_profile = Some(perigee_sriov::config::SriovProfileConfig {
                        mac,
                        num_vfs: pf.max_vfs.min(16),
                        mac_strategy: perigee_sriov::config::MacStrategyConfig::Sequential,
                        defaults: perigee_sriov::config::VfDefaults::default(),
                        vf: Vec::new(),
                        fdb: perigee_sriov::config::FdbConfig::default(),
                    });
                }
                if state.sriov_state.editing_name.is_empty() {
                    state.sriov_state.editing_name = pf.iface_name.clone();
                }
            }
        }
        KeyCode::Char('r') => {
            state.sriov_state.scan_pfs();
        }
        KeyCode::Char(c) if !c.is_control() => {
            state.sriov_state.editing_name.push(c);
        }
        KeyCode::Backspace => {
            state.sriov_state.editing_name.pop();
        }
        _ => {}
    }
}
