use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::{common, AppScreen, AppState};

fn menu_items() -> Vec<(&'static str, &'static str)> {
    vec![
        perigee_sriov::module_info(),
        // perigee_gpu::module_info(),  // future
    ]
}

const LOGO: &[&str] = &[
    r"  ██████  ███████ ██████  ██  ██████  ███████ ███████ ",
    r"  ██   ██ ██      ██   ██ ██ ██       ██      ██      ",
    r"  ██████  █████   ██████  ██ ██   ███ █████   █████   ",
    r"  ██      ██      ██   ██ ██ ██    ██ ██      ██      ",
    r"  ██      ███████ ██   ██ ██  ██████  ███████ ███████ ",
];

const BOX_WIDTH: u16 = 66;

pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    let has_pve = state.host_info.pve_version.is_some();
    let info_rows: u16 = if has_pve { 5 } else { 4 };
    let info_box_h = info_rows + 2; // rows + border top/bottom

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),          // header
            Constraint::Length(1),          // spacing
            Constraint::Length(5),          // logo
            Constraint::Length(1),          // subtitle
            Constraint::Length(1),          // gap
            Constraint::Length(info_box_h), // system info
            Constraint::Length(1),          // gap
            Constraint::Min(0),            // modules + fill
            Constraint::Length(2),          // footer
        ])
        .split(area);

    common::header_bar(frame, chunks[0], "Home", state.daemon_online);

    // Logo
    let logo_lines: Vec<Line> = LOGO
        .iter()
        .map(|line| {
            Line::from(Span::styled(
                *line,
                Style::default()
                    .fg(common::BRAND)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    let logo_widget = Paragraph::new(logo_lines).alignment(Alignment::Center);
    frame.render_widget(logo_widget, chunks[2]);

    // Subtitle
    let subtitle = Paragraph::new(Line::from(vec![
        Span::styled(
            "Proxmox VE Helper Toolkit",
            Style::default().fg(common::TEXT_MUTED),
        ),
        Span::styled(
            format!("  v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(common::BORDER),
        ),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(subtitle, chunks[3]);

    // Shared width for both boxes
    let box_w = BOX_WIDTH.min(area.width.saturating_sub(4));
    // Max chars for value column (box_w - borders(2) - indent(2) - label(14))
    let val_max = box_w.saturating_sub(2 + 2 + 14) as usize;

    // ── System info box ──
    let info_area = centered_h(box_w, chunks[5]);

    let hi = &state.host_info;
    let mut info_lines: Vec<Line> = Vec::new();

    let loading = hi.hostname.is_empty();
    if loading {
        info_lines.push(Line::from(Span::styled(
            "  Loading system info...",
            common::style_muted(),
        )));
    } else {
        let kv = |label: &str, value: &str| -> Line<'static> {
            let truncated = truncate_str(value, val_max);
            Line::from(vec![
                Span::styled(format!("  {:<14}", label), common::style_label()),
                Span::styled(truncated, common::style_value()),
            ])
        };

        info_lines.push(kv("Hostname:", &hi.hostname));
        info_lines.push(kv("Kernel:", &hi.kernel));
        if let Some(pve) = &hi.pve_version {
            info_lines.push(kv("Proxmox VE:", pve));
        }
        info_lines.push(kv(
            "CPU:",
            &if hi.cpu_cores > 0 {
                format!("{} ({}C)", hi.cpu_model, hi.cpu_cores)
            } else {
                hi.cpu_model.clone()
            },
        ));
        info_lines.push(kv("Memory:", &hi.memory_str()));
    }
    let info_block = Paragraph::new(info_lines).block(
        Block::default()
            .title(Span::styled(
                " System ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(info_block, info_area);

    // ── Module list ── (same width as system info)
    let modules = menu_items();
    let list_height = (modules.len() as u16 * 2 + 2).min(chunks[7].height);
    let list_area = centered_h(box_w, chunks[7]);
    let list_area = Rect {
        height: list_height,
        ..list_area
    };

    let items: Vec<ListItem> = modules
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| {
            let is_selected = i == 0;
            let prefix = if is_selected { " ▸ " } else { "   " };
            let name_style = if is_selected {
                common::style_selected()
            } else {
                Style::default().fg(common::TEXT_DIM)
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(prefix, name_style),
                    Span::styled(name.to_string(), name_style),
                ]),
                Line::from(Span::styled(
                    format!("     {}", desc),
                    common::style_muted(),
                )),
            ])
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(Span::styled(
                " Modules ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(list, list_area);

    common::footer_bar(
        frame,
        chunks[8],
        &[("Enter", "Select"), ("q", "Quit")],
    );
}

fn centered_h(width: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    Rect::new(x, area.y, width.min(area.width), area.height)
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}

pub async fn handle_input(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => state.should_quit = true,
        KeyCode::Enter => {
            state.screen = AppScreen::SriovProfiles;
            state.sriov_state.load_profiles();
            state.sriov_state.fetch_profile_statuses().await;
        }
        _ => {}
    }
}
