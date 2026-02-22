use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
    Frame,
};

use super::{common, AppScreen, AppState};

const MENU_ITEMS: &[(&str, &str)] = &[
    ("SR-IOV", "Configure SR-IOV virtual functions"),
    // Future: ("GPU Passthrough", "Configure GPU passthrough"),
    // Future: ("ZFS Tuning", "Optimize ZFS parameters"),
];

pub fn render(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "Main Menu", state.daemon_online);

    let items: Vec<ListItem> = MENU_ITEMS
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| {
            let prefix = if i == 0 { "▸ " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{}{}", prefix, name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Modules ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(list, chunks[1]);

    common::footer_bar(
        frame,
        chunks[2],
        &[("Enter", "Select"), ("q", "Quit")],
    );
}

pub fn handle_input(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => state.should_quit = true,
        KeyCode::Enter => {
            state.screen = AppScreen::SriovProfiles;
            state.sriov_state.load_profiles();
        }
        _ => {}
    }
}
