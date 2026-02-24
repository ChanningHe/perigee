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
        perigee_affinity::module_info(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonState {
    Running,
    Stopped,
    NotInstalled,
}

fn probe_daemon_state() -> DaemonState {
    use std::process::Command;
    let output = Command::new("systemctl")
        .args(["is-active", "perigee.service"])
        .output();
    match output {
        Ok(o) => {
            let status = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if status == "active" {
                DaemonState::Running
            } else {
                let unit_check = Command::new("systemctl")
                    .args(["cat", "perigee.service"])
                    .output();
                match unit_check {
                    Ok(u) if u.status.success() => DaemonState::Stopped,
                    _ => DaemonState::NotInstalled,
                }
            }
        }
        Err(_) => DaemonState::NotInstalled,
    }
}

fn daemon_action(action: &str) -> Result<String, String> {
    use std::process::Command;
    let output = Command::new("systemctl")
        .args([action, "perigee.service"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(format!("perigee.service {}", action))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("systemctl {} failed: {}", action, stderr))
    }
}

pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    let has_pve = state.host_info.pve_version.is_some();
    let info_rows: u16 = if has_pve { 5 } else { 4 };
    let info_box_h = info_rows + 2;

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
            Constraint::Length(4),          // daemon status
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
            format!("  v{}", env!("PERIGEE_VERSION_STRING")),
            Style::default().fg(common::BORDER),
        ),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(subtitle, chunks[3]);

    let box_w = BOX_WIDTH.min(area.width.saturating_sub(4));
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

    // ── Daemon status box ──
    let daemon_area = centered_h(box_w, chunks[7]);
    let ds = probe_daemon_state();
    let (status_icon, status_text, status_color) = match ds {
        DaemonState::Running => ("●", "Running", common::SUCCESS),
        DaemonState::Stopped => ("○", "Stopped", common::WARN),
        DaemonState::NotInstalled => ("✗", "Not Installed", common::ERROR),
    };

    let hint = match ds {
        DaemonState::Running => "(s) stop  (r) restart",
        DaemonState::Stopped => "(s) start",
        DaemonState::NotInstalled => "(i) install",
    };

    let daemon_lines = vec![Line::from(vec![
        Span::styled("  Status: ", common::style_label()),
        Span::styled(
            format!("{} {}", status_icon, status_text),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("    {}", hint), Style::default().fg(common::TEXT_DIM)),
    ])];

    if let Some(ref msg) = state.daemon_message {
        // not shown inline for now — could add a second line
        let _ = msg;
    }

    let daemon_block = Paragraph::new(daemon_lines).block(
        Block::default()
            .title(Span::styled(
                " Daemon ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(daemon_block, daemon_area);

    // ── Module list ──
    let modules = menu_items();
    let list_height = (modules.len() as u16 * 2 + 2).min(chunks[9].height);
    let list_area = centered_h(box_w, chunks[9]);
    let list_area = Rect {
        height: list_height,
        ..list_area
    };

    let items: Vec<ListItem> = modules
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| {
            let is_selected = i == state.home_cursor;
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
        chunks[10],
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
    let module_count = menu_items().len();
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => state.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => {
            if module_count > 0 {
                state.home_cursor = if state.home_cursor == 0 {
                    module_count - 1
                } else {
                    state.home_cursor - 1
                };
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if module_count > 0 {
                state.home_cursor = (state.home_cursor + 1) % module_count;
            }
        }
        KeyCode::Enter => match state.home_cursor {
            0 => {
                state.screen = AppScreen::SriovProfiles;
                state.sriov_state.load_profiles();
                state.sriov_state.fetch_profile_statuses().await;
            }
            1 => {
                if !state.affinity_state.data_ready {
                    state.affinity_state.detect_topology();
                    state.affinity_state.refresh_vms();
                }
                state.screen = AppScreen::AffinityTopology;
            }
            _ => {}
        },
        KeyCode::Char('s') => {
            let ds = probe_daemon_state();
            let result = match ds {
                DaemonState::Running => daemon_action("stop"),
                DaemonState::Stopped => daemon_action("start"),
                _ => return,
            };
            match result {
                Ok(msg) => state.daemon_message = Some(msg),
                Err(e) => state.daemon_message = Some(e),
            }
            state.daemon_online = perigee_core::client::IpcClient::is_daemon_running();
        }
        KeyCode::Char('r') if probe_daemon_state() == DaemonState::Running => {
            match daemon_action("restart") {
                Ok(msg) => state.daemon_message = Some(msg),
                Err(e) => state.daemon_message = Some(e),
            }
            state.daemon_online = perigee_core::client::IpcClient::is_daemon_running();
        }
        KeyCode::Char('i') if probe_daemon_state() == DaemonState::NotInstalled => {
            match crate::install::install(true) {
                Ok(()) => state.daemon_message = Some("Installed successfully".to_string()),
                Err(e) => state.daemon_message = Some(format!("Install failed: {}", e)),
            }
            state.daemon_online = perigee_core::client::IpcClient::is_daemon_running();
        }
        _ => {}
    }
}
