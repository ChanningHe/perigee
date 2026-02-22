use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ui::{common, AppState};
use perigee_sriov::config::FdbMode;

const FDB_MODE_COUNT: usize = 3;

struct FdbModeInfo {
    mode: FdbMode,
    label: &'static str,
    desc: &'static str,
}

const FDB_MODES: [FdbModeInfo; 3] = [
    FdbModeInfo {
        mode: FdbMode::DaemonWatch,
        label: "Daemon Watch (recommended)",
        desc: "Monitors /etc/pve/ and updates bridge FDB in real-time.",
    },
    FdbModeInfo {
        mode: FdbMode::Hookscript,
        label: "Hookscript",
        desc: "Generate hookscript for manual VM attachment.",
    },
    FdbModeInfo {
        mode: FdbMode::Disabled,
        label: "Disabled",
        desc: "No FDB management.",
    },
];

pub fn render(frame: &mut Frame, state: &AppState, area: Rect) {
    let current_mode = state
        .sriov_state
        .editing_profile
        .as_ref()
        .map(|p| &p.fdb.mode)
        .unwrap_or(&FdbMode::DaemonWatch);

    let cursor = state.sriov_state.fdb_cursor;

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "   FDB Management Mode:",
        Style::default().fg(common::TEXT),
    )));
    lines.push(Line::from(""));

    for (i, info) in FDB_MODES.iter().enumerate() {
        let is_current = *current_mode == info.mode;
        let is_cursor = i == cursor;
        let radio = if is_current { "●" } else { "○" };

        let indicator = if is_cursor { " ▸ " } else { "   " };
        let label_style = if is_cursor {
            common::style_selected()
        } else if is_current {
            common::style_value()
        } else {
            Style::default().fg(common::TEXT_DIM)
        };

        lines.push(Line::from(vec![
            Span::styled(indicator.to_string(), label_style),
            Span::styled(
                format!("{} ", radio),
                Style::default().fg(if is_current {
                    common::SUCCESS
                } else {
                    common::TEXT_MUTED
                }),
            ),
            Span::styled(info.label.to_string(), label_style),
        ]));
        lines.push(Line::from(Span::styled(
            format!("      {}", info.desc),
            common::style_muted(),
        )));
        lines.push(Line::from(""));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

pub fn handle_input(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if state.sriov_state.fdb_cursor > 0 {
                state.sriov_state.fdb_cursor -= 1;
            } else {
                state.sriov_state.fdb_cursor = FDB_MODE_COUNT - 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.sriov_state.fdb_cursor = (state.sriov_state.fdb_cursor + 1) % FDB_MODE_COUNT;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(ref mut profile) = state.sriov_state.editing_profile {
                profile.fdb.mode = FDB_MODES[state.sriov_state.fdb_cursor].mode.clone();
            }
        }
        _ => {}
    }
}
