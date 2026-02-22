use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn header_bar(frame: &mut Frame, area: Rect, title: &str, daemon_online: bool) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(16)])
        .split(area);

    let title_widget = Paragraph::new(Line::from(vec![
        Span::styled("  Perigee", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" > "),
        Span::styled(title, Style::default().fg(Color::White)),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));

    let daemon_status = if daemon_online {
        Span::styled("daemon: ● on ", Style::default().fg(Color::Green))
    } else {
        Span::styled("daemon: ○ off", Style::default().fg(Color::DarkGray))
    };
    let status_widget = Paragraph::new(Line::from(daemon_status))
        .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(title_widget, chunks[0]);
    frame.render_widget(status_widget, chunks[1]);
}

pub fn footer_bar(frame: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    let spans: Vec<Span> = hints
        .iter()
        .flat_map(|(key, desc)| {
            vec![
                Span::styled(
                    format!(" {} ", key),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::DarkGray),
                ),
                Span::styled(format!(" {} ", desc), Style::default().fg(Color::Gray)),
                Span::raw(" "),
            ]
        })
        .collect();

    let footer = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, area);
}

/// Center a rectangle within a given area.
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
