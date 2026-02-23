use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

// ── Unified color palette ──

pub const BRAND: Color = Color::Rgb(0, 215, 255);
pub const BRAND_DIM: Color = Color::Rgb(0, 135, 175);
pub const TEXT: Color = Color::White;
pub const TEXT_DIM: Color = Color::Gray;
pub const TEXT_MUTED: Color = Color::DarkGray;
pub const LABEL: Color = Color::Rgb(140, 140, 160);
pub const SELECTED: Color = Color::Rgb(0, 215, 255);
pub const EDITING: Color = Color::Rgb(255, 215, 0);
pub const SUCCESS: Color = Color::Rgb(80, 250, 123);
pub const WARN: Color = Color::Rgb(255, 183, 77);
pub const ERROR: Color = Color::Rgb(255, 85, 85);
pub const BORDER: Color = Color::Rgb(68, 68, 90);
pub const SURFACE: Color = Color::Rgb(40, 42, 54);
pub const KEY_BG: Color = Color::Rgb(58, 62, 82);
pub const KEY_FG: Color = Color::Rgb(180, 190, 220);
pub const OVERRIDE_MARK: Color = Color::Rgb(189, 147, 249);

// ── Style helpers ──

pub fn style_selected() -> Style {
    Style::default().fg(SELECTED).add_modifier(Modifier::BOLD)
}

pub fn style_editing() -> Style {
    Style::default().fg(EDITING).add_modifier(Modifier::BOLD)
}

pub fn style_label() -> Style {
    Style::default().fg(LABEL)
}

pub fn style_value() -> Style {
    Style::default().fg(TEXT)
}

pub fn style_muted() -> Style {
    Style::default().fg(TEXT_MUTED)
}

pub fn style_success() -> Style {
    Style::default().fg(SUCCESS)
}

pub fn style_warn() -> Style {
    Style::default().fg(WARN)
}

pub fn style_error() -> Style {
    Style::default().fg(ERROR)
}

// ── Header ──

pub fn header_bar(frame: &mut Frame, area: Rect, title: &str, daemon_online: bool) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(16)])
        .split(area);

    let title_widget = Paragraph::new(Line::from(vec![
        Span::styled(
            "  Perigee",
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" › ", Style::default().fg(TEXT_MUTED)),
        Span::styled(title, Style::default().fg(TEXT)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(BORDER)),
    );

    let daemon_status = if daemon_online {
        Span::styled(
            " ● daemon on ",
            Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" ○ daemon off", Style::default().fg(TEXT_MUTED))
    };
    let status_widget = Paragraph::new(Line::from(daemon_status)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(BORDER)),
    );

    frame.render_widget(title_widget, chunks[0]);
    frame.render_widget(status_widget, chunks[1]);
}

// ── Footer ──

pub fn footer_bar(frame: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    let spans: Vec<Span> = hints
        .iter()
        .flat_map(|(key, desc)| {
            vec![
                Span::styled(
                    format!(" {} ", key),
                    Style::default()
                        .fg(KEY_FG)
                        .bg(KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {} ", desc), Style::default().fg(TEXT_DIM)),
                Span::raw("  "),
            ]
        })
        .collect();

    let footer = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(BORDER)),
    );
    frame.render_widget(footer, area);
}

// ── Utility ──

pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

pub fn state_color(state: &perigee_core::ipc::ProfileState) -> Color {
    match state {
        perigee_core::ipc::ProfileState::Active => SUCCESS,
        perigee_core::ipc::ProfileState::Degraded => WARN,
        perigee_core::ipc::ProfileState::Pending => BRAND,
        perigee_core::ipc::ProfileState::NicOffline => TEXT_MUTED,
        perigee_core::ipc::ProfileState::Error => ERROR,
    }
}
