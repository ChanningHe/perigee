use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::AppState;

pub fn render(frame: &mut Frame, state: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Top half: summary
    render_summary(frame, state, chunks[0]);
    // Bottom half: TOML preview
    render_toml_preview(frame, state, chunks[1]);
}

fn render_summary(frame: &mut Frame, state: &AppState, area: Rect) {
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
        let name = &state.sriov_state.editing_name;
        let name_ok = !name.trim().is_empty();
        let mac_ok = profile.mac.to_string() != "00:00:00:00:00:00";
        let vf_ok = profile.num_vfs > 0;

        let check = |ok: bool| -> Span<'static> {
            if ok {
                Span::styled("  ✓ ", Style::default().fg(Color::Green))
            } else {
                Span::styled("  ✗ ", Style::default().fg(Color::Red))
            }
        };

        lines.push(Line::from(vec![
            check(name_ok),
            Span::styled("Profile:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if name_ok {
                    name.to_string()
                } else {
                    "(empty - required)".to_string()
                },
                Style::default().fg(if name_ok { Color::White } else { Color::Red }),
            ),
        ]));
        lines.push(Line::from(vec![
            check(mac_ok),
            Span::styled("PF MAC:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                profile.mac.to_string(),
                Style::default().fg(if mac_ok { Color::White } else { Color::Red }),
            ),
        ]));
        lines.push(Line::from(vec![
            check(vf_ok),
            Span::styled("VF Count:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                profile.num_vfs.to_string(),
                Style::default().fg(if vf_ok { Color::White } else { Color::Red }),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("MAC:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:?}", profile.mac_strategy),
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("Trust:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if profile.defaults.trust { "on" } else { "off" },
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("SpoofChk:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if profile.defaults.spoofchk { "on" } else { "off" },
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("Overrides:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} VFs", profile.vf.len()),
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("FDB:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:?}", profile.fdb.mode),
                Style::default().fg(Color::White),
            ),
        ]));

        lines.push(Line::from(""));

        let all_ok = name_ok && mac_ok && vf_ok;
        if all_ok {
            lines.push(Line::from(Span::styled(
                "  Press Ctrl+S or Enter to Save & Apply",
                Style::default().fg(Color::Green),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "  ✗ Fix required fields before saving (marked with ✗ above)",
                Style::default().fg(Color::Red),
            )));
        }

        if let Some(msg) = &state.sriov_state.message {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {}", msg),
                Style::default().fg(Color::Yellow),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  No profile configured yet. Fill in PF and General tabs first.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" Summary ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, area);
}

fn render_toml_preview(frame: &mut Frame, state: &AppState, area: Rect) {
    let lines = if let Some(profile) = &state.sriov_state.editing_profile {
        let name = if state.sriov_state.editing_name.trim().is_empty() {
            "unnamed"
        } else {
            state.sriov_state.editing_name.trim()
        };

        let mut map = std::collections::BTreeMap::new();
        map.insert(name.to_string(), profile.clone());
        let file_config = perigee_sriov::config::SriovFileConfig { sriov: map };

        match toml::to_string_pretty(&file_config) {
            Ok(toml_str) => toml_str
                .lines()
                .map(|l| {
                    Line::from(Span::styled(
                        format!("  {}", l),
                        Style::default().fg(Color::Gray),
                    ))
                })
                .collect(),
            Err(e) => vec![Line::from(Span::styled(
                format!("  Serialize error: {}", e),
                Style::default().fg(Color::Red),
            ))],
        }
    } else {
        vec![Line::from(Span::styled(
            "  (no config to preview)",
            Style::default().fg(Color::DarkGray),
        ))]
    };

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" TOML Preview ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .scroll((0, 0));
    frame.render_widget(para, area);
}

pub async fn handle_input(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            // Trigger save (same as Ctrl+S, handled in mod.rs do_save)
            if state.sriov_state.editing_profile.is_some() {
                match state.sriov_state.save_config() {
                    Ok(()) => {
                        state.sriov_state.message = Some("Config saved.".to_string());
                        if crate::client::IpcClient::is_daemon_running() {
                            let _ = crate::client::IpcClient::send(
                                &perigee_core::ipc::Request::Reload,
                            )
                            .await;
                            state.sriov_state.message =
                                Some("Config saved. Reload sent to daemon.".to_string());
                        }
                    }
                    Err(e) => {
                        state.sriov_state.message = Some(format!("Save failed: {}", e));
                    }
                }
            }
        }
        _ => {}
    }
}
