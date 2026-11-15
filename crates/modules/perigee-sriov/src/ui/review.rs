use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::SriovState;

pub fn render(frame: &mut Frame, sriov: &mut SriovState, area: Rect) {
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
            Span::styled("Link State: ", common::style_label()),
            Span::styled(
                profile
                    .defaults
                    .link_state
                    .map_or("unmanaged", |s| s.as_str()),
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
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled("RoCE QoS:   ", common::style_label()),
            Span::styled(roce_summary(profile.roce.as_ref()), common::style_value()),
        ]));
        if profile.roce_enabled() {
            lines.push(Line::from(Span::styled(
                format!("    {}", super::roce_config::PFC_FLAP_WARNING),
                common::style_warn(),
            )));
        }
        // Edited on the General tab; shown when managed.
        if let Some(rx) = profile.rx_ring {
            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled("RX ring:    ", common::style_label()),
                Span::styled(rx.to_string(), common::style_value()),
            ]));
        }

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

/// One-line view of the RoCE section; edited on the RoCE tab.
fn roce_summary(roce: Option<&crate::config::RoceConfig>) -> String {
    match roce {
        None => "not configured".to_string(),
        Some(r) if !r.enabled => "off".to_string(),
        Some(r) => format!(
            "on — DSCP {}→prio {} (PFC), CNP {}→prio {}, storm {} ms, ToS {}",
            r.dscp,
            r.priority,
            r.cnp_dscp,
            r.cnp_priority,
            r.pfc_storm_timeout_ms,
            r.roce_tos()
                .map_or_else(|| "invalid".to_string(), |t| t.to_string())
        ),
    }
}

fn render_toml_preview(frame: &mut Frame, sriov: &mut SriovState, area: Rect) {
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

    // Clamp scroll to content height (borders take 2 rows) so a long preview
    // (many VF overrides) stays reachable instead of clipping silently.
    let viewport = area.height.saturating_sub(2);
    let max_scroll = (lines.len() as u16).saturating_sub(viewport);
    sriov.review_scroll = sriov.review_scroll.min(max_scroll);

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
        .scroll((sriov.review_scroll, 0));
    frame.render_widget(para, area);
}

pub fn handle_input(sriov: &mut SriovState, key: KeyEvent) {
    // Enter / Ctrl+S on the Review tab are handled by handle_editor_input;
    // here we only scroll the TOML preview. Upper bound is clamped in render.
    match key.code {
        KeyCode::Up => sriov.review_scroll = sriov.review_scroll.saturating_sub(1),
        KeyCode::Down => sriov.review_scroll = sriov.review_scroll.saturating_add(1),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::SriovState;
    use crate::config::SriovFileConfig;

    const WITH_RING: &str =
        "[sriov.ring]\nmac = \"8c:2a:8e:88:9a:f6\"\nnum_vfs = 4\nrx_ring = 8192\n\n\
                             [sriov.plain]\nmac = \"8c:2a:8e:88:9a:f7\"\nnum_vfs = 2\n";

    /// Save one profile through the TUI path and reload the whole file.
    fn save_and_reload(edit: &str) -> (SriovFileConfig, String) {
        let dir = std::env::temp_dir().join(format!("perigee-ring-{edit}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sriov.toml");
        std::fs::write(&path, WITH_RING).unwrap();
        let mut sriov = SriovState::new();
        sriov.editing_name = edit.to_string();
        sriov.editing_profile = Some(SriovFileConfig::load(&path).unwrap().sriov[edit].clone());
        sriov.editing_profile.as_mut().unwrap().num_vfs = 8;
        sriov.save_config_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let saved = SriovFileConfig::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        (saved, text)
    }

    #[test]
    fn rx_ring_survives_a_tui_save() {
        let (saved, _) = save_and_reload("ring");
        assert_eq!(saved.sriov["ring"].num_vfs, 8, "the edit itself was saved");
        assert_eq!(saved.sriov["ring"].rx_ring, Some(8192));
        assert_eq!(saved.sriov["plain"].rx_ring, None);
    }

    #[test]
    fn absent_rx_ring_stays_absent_after_a_tui_save() {
        let (saved, text) = save_and_reload("plain");
        assert_eq!(saved.sriov["plain"].rx_ring, None);
        assert_eq!(
            text.matches("rx_ring").count(),
            1,
            "only the other profile's: {text}"
        );
    }
}
