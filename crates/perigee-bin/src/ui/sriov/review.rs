use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::AppState;

pub fn render(frame: &mut Frame, state: &AppState, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(
            "  Configuration Review",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if let Some(profile) = &state.sriov_state.editing_profile {
        lines.extend(vec![
            Line::from(vec![
                Span::styled("  Profile:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &state.sriov_state.editing_name,
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  PF MAC:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    profile.mac.to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  VF Count:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    profile.num_vfs.to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  MAC:        ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:?}", profile.mac_strategy),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Trust:      ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    if profile.defaults.trust { "on" } else { "off" },
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  SpoofChk:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    if profile.defaults.spoofchk { "on" } else { "off" },
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Overrides:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} VFs", profile.vf.len()),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("  FDB:        ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:?}", profile.fdb.mode),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  Press Ctrl+S to Save & Apply",
                Style::default().fg(Color::Green),
            )),
        ]);
    } else {
        lines.push(Line::from(Span::styled(
            "  No profile configured yet. Fill in PF and General tabs first.",
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

pub async fn handle_input(_state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            // Same as Ctrl+S: save & apply
            // TODO: implement save logic
        }
        _ => {}
    }
}
