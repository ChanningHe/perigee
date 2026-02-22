use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::AppState;

pub fn render_general(frame: &mut Frame, state: &AppState, area: Rect) {
    let profile = &state.sriov_state.editing_profile;
    let (num_vfs, mac_strategy, trust, spoofchk) = match profile {
        Some(p) => (
            p.num_vfs.to_string(),
            format!("{:?}", p.mac_strategy),
            if p.defaults.trust { "on" } else { "off" },
            if p.defaults.spoofchk { "on" } else { "off" },
        ),
        None => (
            "0".to_string(),
            "Sequential".to_string(),
            "on",
            "off",
        ),
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("  VF Count:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("[{}]", num_vfs), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  MAC Strategy: ", Style::default().fg(Color::DarkGray)),
            Span::styled(mac_strategy, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  ── Default VF Properties ──",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(vec![
            Span::styled("  Trust:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(trust, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  SpoofChk:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(spoofchk, Style::default().fg(Color::White)),
        ]),
    ];

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}

pub fn handle_general_input(_state: &mut AppState, _key: KeyEvent) {
    // TODO: implement field navigation and editing
}

pub fn render_vf_table(frame: &mut Frame, state: &AppState, area: Rect) {
    let profile = &state.sriov_state.editing_profile;

    let mut lines = vec![
        Line::from(Span::styled(
            "  Sel  VF#  MAC Address         Trust SpoofChk VLAN",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  ─────────────────────────────────────────────────",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    if let Some(profile) = profile {
        let num = profile.num_vfs.min(64);
        for i in 0..num {
            let has_override = profile.vf.iter().any(|o| o.index == i);
            let override_marker = if has_override { " *" } else { "" };
            let style = if has_override {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "  [ ] {:>3}   (auto)              {}    {}     {}{}",
                    i,
                    if profile.defaults.trust { "✓" } else { "✗" },
                    if profile.defaults.spoofchk { "✓" } else { "✗" },
                    "-",
                    override_marker,
                ),
                style,
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  (configure VF count in General tab first)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}

pub fn handle_vf_table_input(_state: &mut AppState, _key: KeyEvent) {
    // TODO: implement VF table interaction (select, edit, batch)
}
