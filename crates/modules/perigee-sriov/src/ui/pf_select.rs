use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::SriovState;
use crate::pf_ring::RingState;

pub fn render(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let is_editing_name = sriov.edit_focus == Some(super::EditFocus::ProfileName);

    let name_style = if is_editing_name {
        common::style_editing()
    } else {
        common::style_value()
    };

    let name_display = if sriov.editing_name.is_empty() && !is_editing_name {
        Line::from(vec![
            Span::styled("  Profile Name: ", common::style_label()),
            Span::styled("[", common::style_muted()),
            Span::styled(
                "type a name or select PF below",
                Style::default().fg(common::TEXT_MUTED),
            ),
            Span::styled("]", common::style_muted()),
        ])
    } else {
        Line::from(vec![
            Span::styled("  Profile Name: ", common::style_label()),
            Span::styled(
                format!(
                    "[{}{}]",
                    sriov.editing_name,
                    if is_editing_name { "▎" } else { "" }
                ),
                name_style,
            ),
        ])
    };

    let name_block = Paragraph::new(name_display).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(name_block, chunks[0]);

    let pfs = &sriov.detected_pfs;
    let selected = sriov.selected_pf;

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        "  Select Physical Function:",
        Style::default().fg(common::TEXT),
    )));
    lines.push(Line::from(""));

    if let Some(err) = &sriov.pf_scan_error {
        lines.push(Line::from(Span::styled(
            format!("  Scan error: {}", err),
            common::style_error(),
        )));
    } else if pfs.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No SR-IOV capable PFs detected.",
            common::style_warn(),
        )));
        lines.push(Line::from(Span::styled(
            "  Check: IOMMU enabled, SR-IOV in BIOS, driver loaded.",
            common::style_muted(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "  {:<3}{:<14} {:<14} {:<20} {:<12} {:>7}",
                "", "Iface", "PCI Address", "MAC", "Vendor", "VFs"
            ),
            common::style_muted(),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(73)),
            Style::default().fg(common::BORDER),
        )));

        for (i, pf) in pfs.iter().enumerate() {
            let is_selected = i == selected;
            let prefix = if is_selected { " ▸ " } else { "   " };
            let style = if is_selected {
                common::style_selected()
            } else {
                Style::default().fg(common::TEXT_DIM)
            };

            let vf_str = format!("{}/{}", pf.current_vfs, pf.max_vfs);
            lines.push(Line::from(Span::styled(
                format!(
                    "{}{:<14} {:<14} {:<20} {:<12} {:>7}",
                    prefix, pf.iface_name, pf.pci_address, pf.mac_address, pf.vendor, vf_str,
                ),
                style,
            )));
        }

        if let Some(pf) = pfs.get(selected) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ── Selected PF ──",
                common::style_muted(),
            )));
            lines.push(Line::from(vec![
                Span::styled("  Driver: ", common::style_label()),
                Span::styled(&pf.driver, common::style_value()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Link:   ", common::style_label()),
                Span::styled(
                    format!(
                        "{}{}",
                        pf.link_state,
                        pf.speed
                            .as_ref()
                            .map(|s| format!("  Speed: {}Mb/s", s))
                            .unwrap_or_default()
                    ),
                    Style::default().fg(if pf.link_state == crate::detect::LinkState::Up {
                        common::SUCCESS
                    } else {
                        common::WARN
                    }),
                ),
            ]));
        }
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, chunks[1]);
}

/// Adopt the highlighted PF into the profile and probe its RX ring maximum
/// once, here, so the General tab never does I/O while rendering.
fn select_pf(sriov: &mut SriovState, probe: &dyn Fn(&str) -> Result<RingState, String>) {
    let Some(pf) = sriov.detected_pfs.get(sriov.selected_pf) else {
        return;
    };
    let mac = pf.mac_address;
    let iface = pf.iface_name.clone();
    if let Some(ref mut profile) = sriov.editing_profile {
        profile.mac = mac;
    } else {
        sriov.editing_profile = Some(crate::config::SriovProfileConfig {
            mac,
            num_vfs: pf.max_vfs.min(16),
            mac_strategy: crate::config::MacStrategyConfig::Sequential,
            defaults: crate::config::VfDefaults::default(),
            vf: Vec::new(),
            fdb: crate::config::FdbConfig::default(),
            roce: None,
            rx_ring: None,
        });
    }
    if sriov.editing_name.is_empty() {
        sriov.editing_name = iface.clone();
    }
    sriov.set_pf_ring_with(Some(&iface), probe);
}

pub fn handle_input(sriov: &mut SriovState, key: KeyEvent) {
    handle_input_with(sriov, key, &crate::pf_ring::probe);
}

/// `handle_input` with the ring probe injectable (tests must not run ethtool).
fn handle_input_with(
    sriov: &mut SriovState,
    key: KeyEvent,
    probe: &dyn Fn(&str) -> Result<RingState, String>,
) {
    let pf_count = sriov.detected_pfs.len();

    match key.code {
        // Not match guards: an unmatched 'k'/'j' would fall through to the
        // name-typing arm below.
        KeyCode::Up | KeyCode::Char('k') => {
            sriov.selected_pf = common::wrap_step(sriov.selected_pf, pf_count, false);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            sriov.selected_pf = common::wrap_step(sriov.selected_pf, pf_count, true);
        }
        KeyCode::Enter => select_pf(sriov, probe),
        KeyCode::Char('r') => {
            sriov.scan_pfs();
        }
        KeyCode::Char(c) if !c.is_control() => {
            sriov.editing_name.push(c);
        }
        KeyCode::Backspace => {
            sriov.editing_name.pop();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{LinkState, PhysicalFunction};
    use crate::ui::vf_config::rx_ring_over_max_warning;
    use crate::vendor::NicVendor;
    use perigee_core::mac::MacAddress;
    use std::cell::{Cell, RefCell};

    fn pf(iface: &str) -> PhysicalFunction {
        PhysicalFunction {
            iface_name: iface.to_string(),
            pci_address: "0000:41:00.0".to_string(),
            mac_address: MacAddress::ZERO,
            vendor: NicVendor::Mellanox,
            max_vfs: 8,
            current_vfs: 0,
            driver: "mlx5_core".to_string(),
            link_state: LinkState::Up,
            speed: None,
        }
    }

    fn state_with_two_pfs() -> SriovState {
        let mut sriov = SriovState::new();
        sriov.detected_pfs = vec![pf("enp1s0"), pf("enp2s0")];
        sriov
    }

    #[test]
    fn choosing_a_pf_probes_that_pfs_interface() {
        let mut sriov = state_with_two_pfs();
        sriov.selected_pf = 1;
        let seen = RefCell::new(Vec::<String>::new());
        let probe = |iface: &str| {
            seen.borrow_mut().push(iface.to_string());
            Ok(RingState {
                current: 512,
                max: 4096,
            })
        };

        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Enter), &probe);

        assert_eq!(seen.into_inner(), vec!["enp2s0".to_string()]);
        assert_eq!(
            sriov.pf_ring_probe,
            Some(Ok(RingState {
                current: 512,
                max: 4096
            }))
        );
    }

    #[test]
    fn choosing_another_pf_replaces_the_cached_probe() {
        let mut sriov = state_with_two_pfs();
        let probe = |iface: &str| {
            Ok(RingState {
                current: 1,
                max: if iface == "enp1s0" { 100 } else { 200 },
            })
        };

        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Enter), &probe);
        sriov.selected_pf = 1;
        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Enter), &probe);

        let max = sriov.pf_ring_probe.and_then(|r| r.ok()).map(|r| r.max);
        assert_eq!(max, Some(200));
    }

    #[test]
    fn a_stored_value_above_the_new_pfs_maximum_is_flagged_not_changed() {
        let mut sriov = state_with_two_pfs();
        let probe = |iface: &str| {
            Ok(RingState {
                current: 1024,
                max: if iface == "enp1s0" { 8192 } else { 1024 },
            })
        };
        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Enter), &probe);
        let stored = Some(8192);
        if let Some(profile) = sriov.editing_profile.as_mut() {
            profile.rx_ring = stored;
        }
        let before = rx_ring_over_max_warning(stored, &sriov.pf_ring_probe);

        sriov.selected_pf = 1;
        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Enter), &probe);

        assert_eq!(before, None, "8192 fits PF A");
        let warning = rx_ring_over_max_warning(stored, &sriov.pf_ring_probe).unwrap_or_default();
        assert!(
            warning.contains("8192") && warning.contains("1024"),
            "{warning}"
        );
        assert_eq!(
            sriov.editing_profile.and_then(|p| p.rx_ring),
            stored,
            "the user's value is never rewritten"
        );
    }

    #[test]
    fn enter_without_detected_pfs_does_not_probe() {
        let mut sriov = SriovState::new();
        let called = Cell::new(false);
        let probe = |_: &str| {
            called.set(true);
            Err("unused".to_string())
        };

        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Enter), &probe);

        assert!(!called.get());
    }

    #[test]
    fn typing_a_name_does_not_probe() {
        let mut sriov = state_with_two_pfs();
        let called = Cell::new(false);
        let probe = |_: &str| {
            called.set(true);
            Err("unused".to_string())
        };

        handle_input_with(&mut sriov, KeyEvent::from(KeyCode::Char('x')), &probe);

        assert!(!called.get());
        assert_eq!(sriov.editing_name, "x");
    }
}
