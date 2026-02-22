use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::AppState;

pub fn render(frame: &mut Frame, state: &AppState, area: Rect) {
    let lines = vec![
        Line::from(vec![
            Span::styled("  Profile Name: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[{}]", &state.sriov_state.editing_name),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Select Physical Function:",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  (PF detection requires running on Proxmox host with SR-IOV hardware)",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  No PFs detected — running in offline/dev mode",
            Style::default().fg(Color::Yellow),
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
    match key.code {
        KeyCode::Char(c) => {
            state.sriov_state.editing_name.push(c);
        }
        KeyCode::Backspace => {
            state.sriov_state.editing_name.pop();
        }
        _ => {}
    }
}
