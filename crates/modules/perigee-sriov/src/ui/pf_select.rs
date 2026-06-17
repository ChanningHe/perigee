use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::SriovState;

pub fn render(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let is_editing_name = sriov.edit_focus == Some(super::EditFocus::ProfileName);

    let name_style = if is_editing_name {
        common::style_editing()
    } else {
        common::style_value()
    };

    let name_display = if sriov.editing_name.is_empty() && !is_editing_name {
        Line::from(vec![
            Span::styled("  Profile Name: ", common::style_label()),
            Span::styled("[", common::style_muted()),
            Span::styled(
                "type a name or select PF below",
                Style::default().fg(common::TEXT_MUTED),
            ),
            Span::styled("]", common::style_muted()),
        ])
    } else {
        Line::from(vec![
            Span::styled("  Profile Name: ", common::style_label()),
            Span::styled(
                format!(
                    "[{}{}]",
                    &sriov.editing_name,
                    if is_editing_name { "▎" } else { "" }
                ),
                name_style,
            ),
        ])
    };

    let name_block = Paragraph::new(name_display).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(name_block, chunks[0]);

    let pfs = &sriov.detected_pfs;
    let selected = sriov.selected_pf;

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "  Select Physical Function:",
        Style::default().fg(common::TEXT),
    )));
    lines.push(Line::from(""));

    if let Some(err) = &sriov.pf_scan_error {
        lines.push(Line::from(Span::styled(
            format!("  Scan error: {}", err),
            common::style_error(),
        )));
    } else if pfs.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No SR-IOV capable PFs detected.",
            common::style_warn(),
        )));
        lines.push(Line::from(Span::styled(
            "  Check: IOMMU enabled, SR-IOV in BIOS, driver loaded.",
            common::style_muted(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<3}{:<14} {:<14} {:<20} {:<12} {:>7}",
                "", "Iface", "PCI Address", "MAC", "Vendor", "VFs"
            ),
            common::style_muted(),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(73)),
            Style::default().fg(common::BORDER),
        )));

        for (i, pf) in pfs.iter().enumerate() {
            let is_selected = i == selected;
            let prefix = if is_selected { " ▸ " } else { "   " };
            let style = if is_selected {
                common::style_selected()
            } else {
                Style::default().fg(common::TEXT_DIM)
            };

            let vf_str = format!("{}/{}", pf.current_vfs, pf.max_vfs);
            lines.push(Line::from(Span::styled(
                format!(
                    "{}{:<14} {:<14} {:<20} {:<12} {:>7}",
                    prefix, pf.iface_name, pf.pci_address, pf.mac_address, pf.vendor, vf_str,
                ),
                style,
            )));
        }

        if let Some(pf) = pfs.get(selected) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ── Selected PF ──",
                common::style_muted(),
            )));
            lines.push(Line::from(vec![
                Span::styled("  Driver: ", common::style_label()),
                Span::styled(&pf.driver, common::style_value()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Link:   ", common::style_label()),
                Span::styled(
                    format!(
                        "{}{}",
                        pf.link_state,
                        pf.speed
                            .as_ref()
                            .map(|s| format!("  Speed: {}Mb/s", s))
                            .unwrap_or_default()
                    ),
                    Style::default().fg(if pf.link_state == crate::detect::LinkState::Up {
                        common::SUCCESS
                    } else {
                        common::WARN
                    }),
                ),
            ]));
        }
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, chunks[1]);
}

pub fn handle_input(sriov: &mut SriovState, key: KeyEvent) {
    let pf_count = sriov.detected_pfs.len();

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if pf_count > 0 {
                if sriov.selected_pf == 0 {
                    sriov.selected_pf = pf_count - 1;
                } else {
                    sriov.selected_pf -= 1;
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if pf_count > 0 {
                sriov.selected_pf = (sriov.selected_pf + 1) % pf_count;
            }
        }
        KeyCode::Enter => {
            if let Some(pf) = sriov.detected_pfs.get(sriov.selected_pf) {
                let mac = pf.mac_address;
                if let Some(ref mut profile) = sriov.editing_profile {
                    profile.mac = mac;
                } else {
                    sriov.editing_profile = Some(crate::config::SriovProfileConfig {
                        mac,
                        num_vfs: pf.max_vfs.min(16),
                        mac_strategy: crate::config::MacStrategyConfig::Sequential,
                        defaults: crate::config::VfDefaults::default(),
                        vf: Vec::new(),
                        fdb: crate::config::FdbConfig::default(),
                    });
                }
                if sriov.editing_name.is_empty() {
                    sriov.editing_name = pf.iface_name.clone();
                }
            }
        }
        KeyCode::Char('r') => {
            sriov.scan_pfs();
        }
        KeyCode::Char(c) if !c.is_control() => {
            sriov.editing_name.push(c);
        }
        KeyCode::Backspace => {
            sriov.editing_name.pop();
        }
        _ => {}
    }
}
