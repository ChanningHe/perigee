use crossterm::event::KeyEvent;
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::SriovState;

pub fn render(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_summary(frame, sriov, chunks[0]);
    render_toml_preview(frame, sriov, chunks[1]);
}

fn render_summary(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(
            "  Configuration Review",
            Style::default()
                .fg(common::TEXT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if let Some(profile) = &sriov.editing_profile {
        let name = &sriov.editing_name;
        let name_ok = !name.trim().is_empty();
        let mac_ok = profile.mac.to_string() != "00:00:00:00:00:00";
        let vf_ok = profile.num_vfs > 0;

        let check = |ok: bool| -> Span<'static> {
            if ok {
                Span::styled("  ✓ ", common::style_success())
            } else {
                Span::styled("  ✗ ", common::style_error())
            }
        };

        lines.push(Line::from(vec![
            check(name_ok),
            Span::styled("Profile:    ", common::style_label()),
            Span::styled(
                if name_ok {
                    name.to_string()
                } else {
                    "(empty - required)".to_string()
                },
                Style::default().fg(if name_ok { common::TEXT } else { common::ERROR }),
            ),
        ]));
        lines.push(Line::from(vec![
            check(mac_ok),
            Span::styled("PF MAC:     ", common::style_label()),
            Span::styled(
                profile.mac.to_string(),
                Style::default().fg(if mac_ok { common::TEXT } else { common::ERROR }),
            ),
        ]));
        lines.push(Line::from(vec![
            check(vf_ok),
            Span::styled("VF Count:   ", common::style_label()),
            Span::styled(
                profile.num_vfs.to_string(),
                Style::default().fg(if vf_ok { common::TEXT } else { common::ERROR }),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("MAC:        ", common::style_label()),
            Span::styled(format!("{:?}", profile.mac_strategy), common::style_value()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("Trust:      ", common::style_label()),
            Span::styled(
                if profile.defaults.trust { "on" } else { "off" },
                common::style_value(),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("SpoofChk:   ", common::style_label()),
            Span::styled(
                if profile.defaults.spoofchk {
                    "on"
                } else {
                    "off"
                },
                common::style_value(),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("Overrides:  ", common::style_label()),
            Span::styled(format!("{} VFs", profile.vf.len()), common::style_value()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("FDB:        ", common::style_label()),
            Span::styled(format!("{:?}", profile.fdb.mode), common::style_value()),
        ]));

        lines.push(Line::from(""));

        let all_ok = name_ok && mac_ok && vf_ok;
        if all_ok {
            let key_style = Style::default()
                .fg(common::KEY_FG)
                .bg(common::KEY_BG)
                .add_modifier(Modifier::BOLD);
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(" Ctrl+S ", key_style),
                Span::styled(" Save config only  ", common::style_muted()),
                Span::styled(" Enter ", key_style),
                Span::styled(" Save & Apply to system", common::style_muted()),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                "  ✗ Fix required fields before saving (marked with ✗ above)",
                common::style_error(),
            )));
        }

        if let Some(msg) = &sriov.message {
            lines.push(Line::from(""));
            let msg_style = if msg.starts_with('✓') {
                common::style_success()
            } else {
                common::style_warn()
            };
            lines.push(Line::from(Span::styled(format!("  {}", msg), msg_style)));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  No profile configured yet. Fill in PF and General tabs first.",
            common::style_muted(),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .title(Span::styled(
                " Summary ",
                Style::default().fg(common::BRAND_DIM),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

fn render_toml_preview(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let lines = if let Some(profile) = &sriov.editing_profile {
        let name = if sriov.editing_name.trim().is_empty() {
            "unnamed"
        } else {
            sriov.editing_name.trim()
        };

        let mut map = std::collections::BTreeMap::new();
        map.insert(name.to_string(), profile.clone());
        let file_config = crate::config::SriovFileConfig { sriov: map };

        match toml::to_string_pretty(&file_config) {
            Ok(toml_str) => toml_str
                .lines()
                .map(|l| {
                    Line::from(Span::styled(
                        format!("  {}", l),
                        Style::default().fg(common::TEXT_DIM),
                    ))
                })
                .collect(),
            Err(e) => vec![Line::from(Span::styled(
                format!("  Serialize error: {}", e),
                common::style_error(),
            ))],
        }
    } else {
        vec![Line::from(Span::styled(
            "  (no config to preview)",
            common::style_muted(),
        ))]
    };

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(Span::styled(
                    " TOML Preview ",
                    Style::default().fg(common::BRAND_DIM),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(common::BORDER)),
        )
        .scroll((0, 0));
    frame.render_widget(para, area);
}

pub fn handle_input(_sriov: &mut SriovState, _key: KeyEvent) {
    // Enter on Review tab is handled by handle_editor_input (save & apply)
}
