use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::AppState;
use perigee_sriov::config::FdbMode;

pub fn render(frame: &mut Frame, state: &AppState, area: Rect) {
    let mode = state
        .sriov_state
        .editing_profile
        .as_ref()
        .map(|p| &p.fdb.mode)
        .unwrap_or(&FdbMode::DaemonWatch);

    let (dw, hs, dis) = match mode {
        FdbMode::DaemonWatch => ("●", "○", "○"),
        FdbMode::Hookscript => ("○", "●", "○"),
        FdbMode::Disabled => ("○", "○", "●"),
    };

    let lines = vec![
        Line::from(Span::styled(
            "  FDB Management Mode:",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {} ", dw), Style::default().fg(Color::Cyan)),
            Span::styled(
                "Daemon Watch (recommended)",
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(Span::styled(
            "    Monitors /etc/pve/ and updates bridge FDB in real-time.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {} ", hs), Style::default().fg(Color::Cyan)),
            Span::styled("Hookscript", Style::default().fg(Color::White)),
        ]),
        Line::from(Span::styled(
            "    Generate hookscript for manual VM attachment.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {} ", dis), Style::default().fg(Color::Cyan)),
            Span::styled("Disabled", Style::default().fg(Color::White)),
        ]),
        Line::from(Span::styled(
            "    No FDB management.",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}

pub fn handle_input(state: &mut AppState, key: KeyEvent) {
    if let Some(profile) = state.sriov_state.editing_profile.as_mut() {
        match key.code {
            KeyCode::Char('1') => profile.fdb.mode = FdbMode::DaemonWatch,
            KeyCode::Char('2') => profile.fdb.mode = FdbMode::Hookscript,
            KeyCode::Char('3') => profile.fdb.mode = FdbMode::Disabled,
            _ => {}
        }
    }
}
