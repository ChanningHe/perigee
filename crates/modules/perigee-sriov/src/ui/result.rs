use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn render_apply_result(frame: &mut Frame, area: Rect, results: &[String]) {
    let mut lines = vec![
        Line::from(Span::styled(
            "  Apply Results",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for result in results {
        let (icon, color) = if result.starts_with("✓") || result.starts_with("OK") {
            ("✓", Color::Green)
        } else if result.starts_with("✗") || result.starts_with("ERR") {
            ("✗", Color::Red)
        } else {
            ("·", Color::Gray)
        };
        lines.push(Line::from(Span::styled(
            format!("  {} {}", icon, result),
            Style::default().fg(color),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}
