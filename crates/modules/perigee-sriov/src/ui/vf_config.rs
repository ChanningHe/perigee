use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use super::{EditFocus, SriovState};
use crate::config::MacStrategyConfig;

// ── General tab fields ──

const GENERAL_FIELDS: usize = 4;
const FIELD_VF_COUNT: usize = 0;
const FIELD_MAC_STRATEGY: usize = 1;
const FIELD_TRUST: usize = 2;
const FIELD_SPOOFCHK: usize = 3;

pub fn render_general(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let profile = &sriov.editing_profile;
    let cursor = sriov.general_cursor;
    let is_editing_vf_count = sriov.edit_focus == Some(EditFocus::GeneralVfCount);

    let vf_count_display = if is_editing_vf_count {
        format!("[{}▎]", &sriov.vf_count_buf)
    } else {
        let val = profile.as_ref().map(|p| p.num_vfs).unwrap_or(0);
        format!("[{}]", val)
    };

    let mac_strategy = profile
        .as_ref()
        .map(|p| match p.mac_strategy {
            MacStrategyConfig::Sequential => "Sequential",
            MacStrategyConfig::Random => "Random",
            MacStrategyConfig::Custom => "Custom",
        })
        .unwrap_or("Sequential");

    let trust = profile.as_ref().map(|p| p.defaults.trust).unwrap_or(true);
    let spoofchk = profile
        .as_ref()
        .map(|p| p.defaults.spoofchk)
        .unwrap_or(false);

    let field_line =
        |idx: usize, label: &str, value: &str, is_editing: bool, hint: &str| -> Line<'static> {
            let is_active = cursor == idx;
            let indicator = if is_active { " ▸ " } else { "   " };
            let val_style = if is_editing {
                common::style_editing()
            } else if is_active {
                common::style_selected()
            } else {
                common::style_value()
            };

            Line::from(vec![
                Span::styled(
                    indicator.to_string(),
                    if is_active {
                        Style::default().fg(common::SELECTED)
                    } else {
                        common::style_muted()
                    },
                ),
                Span::styled(format!("{:<16}", label), common::style_label()),
                Span::styled(value.to_string(), val_style),
                Span::styled(
                    if is_active && !hint.is_empty() {
                        format!("  {}", hint)
                    } else {
                        String::new()
                    },
                    common::style_muted(),
                ),
            ])
        };

    let lines = vec![
        Line::from(""),
        field_line(
            FIELD_VF_COUNT,
            "VF Count:",
            &vf_count_display,
            is_editing_vf_count,
            "Enter to edit",
        ),
        Line::from(""),
        field_line(
            FIELD_MAC_STRATEGY,
            "MAC Strategy:",
            mac_strategy,
            false,
            "Enter to cycle",
        ),
        Line::from(""),
        Line::from(Span::styled(
            "   ── Default VF Properties ──",
            common::style_muted(),
        )),
        Line::from(""),
        field_line(
            FIELD_TRUST,
            "Trust:",
            if trust { "ON" } else { "OFF" },
            false,
            "Enter to toggle",
        ),
        Line::from(""),
        field_line(
            FIELD_SPOOFCHK,
            "SpoofChk:",
            if spoofchk { "ON" } else { "OFF" },
            false,
            "Enter to toggle",
        ),
    ];

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

pub fn handle_general_input(sriov: &mut SriovState, key: KeyEvent) {
    if sriov.edit_focus == Some(EditFocus::GeneralVfCount) {
        match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() => {
                sriov.vf_count_buf.push(c);
            }
            KeyCode::Backspace => {
                sriov.vf_count_buf.pop();
            }
            KeyCode::Enter => {
                if let Ok(n) = sriov.vf_count_buf.parse::<u32>() {
                    if let Some(ref mut profile) = sriov.editing_profile {
                        profile.num_vfs = n;
                    }
                }
                sriov.edit_focus = None;
            }
            KeyCode::Esc => {
                sriov.sync_vf_count_buf();
                sriov.edit_focus = None;
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if sriov.general_cursor > 0 {
                sriov.general_cursor -= 1;
            } else {
                sriov.general_cursor = GENERAL_FIELDS - 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            sriov.general_cursor = (sriov.general_cursor + 1) % GENERAL_FIELDS;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            ensure_editing_profile(sriov);
            match sriov.general_cursor {
                FIELD_VF_COUNT => {
                    sriov.sync_vf_count_buf();
                    sriov.edit_focus = Some(EditFocus::GeneralVfCount);
                }
                FIELD_MAC_STRATEGY => {
                    if let Some(ref mut profile) = sriov.editing_profile {
                        profile.mac_strategy = match profile.mac_strategy {
                            MacStrategyConfig::Sequential => MacStrategyConfig::Random,
                            MacStrategyConfig::Random => MacStrategyConfig::Custom,
                            MacStrategyConfig::Custom => MacStrategyConfig::Sequential,
                        };
                    }
                }
                FIELD_TRUST => {
                    if let Some(ref mut profile) = sriov.editing_profile {
                        profile.defaults.trust = !profile.defaults.trust;
                    }
                }
                FIELD_SPOOFCHK => {
                    if let Some(ref mut profile) = sriov.editing_profile {
                        profile.defaults.spoofchk = !profile.defaults.spoofchk;
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

// ── VF Table tab ──

const VF_TABLE_VISIBLE_ROWS: usize = 20;

pub fn render_vf_table(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let profile = &sriov.editing_profile;

    let inner_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "   {:>3}  {:<14} {:<18} {:<6} {:<9} {:<6} {}",
            "VF#", "PCI Addr", "MAC", "Trust", "SpoofChk", "VLAN", "Override"
        ),
        common::style_muted(),
    )));
    lines.push(Line::from(Span::styled(
        format!("   {}", "─".repeat(72)),
        Style::default().fg(common::BORDER),
    )));

    if let Some(profile) = profile {
        let num = profile.num_vfs.min(128) as usize;
        let cursor = sriov.vf_table_cursor;
        let scroll = sriov.vf_table_scroll;
        let visible_end = (scroll + VF_TABLE_VISIBLE_ROWS).min(num);

        // VFs only have a PCI address once created/applied; before that (and if
        // the PF can't be located) every row shows "-".
        let pf_iface = perigee_core::sysfs::find_iface_by_mac(&profile.mac.to_string()).ok();

        for i in scroll..visible_end {
            let is_selected = i == cursor;
            let vf_override = profile.vf.iter().find(|o| o.index == i as u32);
            let has_override = vf_override.is_some();

            let trust_val = vf_override
                .and_then(|o| o.trust)
                .unwrap_or(profile.defaults.trust);
            let spoof_val = vf_override
                .and_then(|o| o.spoofchk)
                .unwrap_or(profile.defaults.spoofchk);
            let pci_str = pf_iface
                .as_deref()
                .and_then(|pf| perigee_core::sysfs::read_vf_pci_addr(pf, i as u32))
                .unwrap_or_else(|| "-".to_string());
            // Preview the MAC each VF will get from the selected strategy so the
            // operator sees it before applying. Sequential is deterministic
            // (PF MAC + index); Random is assigned at apply time; Custom is not
            // configurable here yet.
            let mac_str = match vf_override.and_then(|o| o.mac.as_ref()) {
                Some(m) => m.to_string(),
                None => match profile.mac_strategy {
                    MacStrategyConfig::Sequential => {
                        profile.mac.increment(i as u64 + 1).to_string()
                    }
                    MacStrategyConfig::Random => "(random)".to_string(),
                    MacStrategyConfig::Custom => "(auto)".to_string(),
                },
            };
            let is_editing_vlan = is_selected && sriov.edit_focus == Some(EditFocus::VfVlanId);
            let vlan_str = if is_editing_vlan {
                format!("[{}▎]", &sriov.vlan_id_buf)
            } else {
                vf_override
                    .and_then(|o| o.vlan.as_ref())
                    .map(|v| format!("{}", v.id))
                    .unwrap_or_else(|| "-".to_string())
            };

            let indicator = if is_selected { " ▸ " } else { "   " };
            let style = if is_editing_vlan {
                common::style_editing()
            } else if is_selected {
                common::style_selected()
            } else if has_override {
                Style::default().fg(common::OVERRIDE_MARK)
            } else {
                Style::default().fg(common::TEXT_DIM)
            };

            let override_mark = if has_override {
                Span::styled(" ●", Style::default().fg(common::OVERRIDE_MARK))
            } else {
                Span::raw("")
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{}{:>3}  {:<14} {:<18} {:<6} {:<9} {:<6}",
                        indicator,
                        i,
                        pci_str,
                        mac_str,
                        if trust_val { "✓" } else { "✗" },
                        if spoof_val { "✓" } else { "✗" },
                        vlan_str,
                    ),
                    style,
                ),
                override_mark,
            ]));
        }

        if num == 0 {
            lines.push(Line::from(Span::styled(
                "   (Set VF count in General tab first)",
                common::style_muted(),
            )));
        }

        if num > VF_TABLE_VISIBLE_ROWS {
            let mut scrollbar_state = ScrollbarState::new(num).position(scroll);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, inner_chunks[1], &mut scrollbar_state);
        }

        lines.push(Line::from(""));
        if sriov.edit_focus == Some(EditFocus::VfVlanId) {
            lines.push(Line::from(vec![
                Span::styled(
                    "   VLAN ID (1-4094), 0/empty to remove.  ",
                    common::style_editing(),
                ),
                Span::styled(
                    " Enter ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" confirm  ", common::style_muted()),
                Span::styled(
                    " Esc ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" cancel", common::style_muted()),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    " t ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" trust  ", common::style_muted()),
                Span::styled(
                    " s ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" spoofchk  ", common::style_muted()),
                Span::styled(
                    " v ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" vlan  ", common::style_muted()),
                Span::styled(
                    " d ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" delete override", common::style_muted()),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "   (Select a PF and configure General settings first)",
            common::style_muted(),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, inner_chunks[0]);
}

pub fn handle_vf_table_input(sriov: &mut SriovState, key: KeyEvent) {
    if sriov.edit_focus == Some(EditFocus::VfVlanId) {
        match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if sriov.vlan_id_buf.len() < 4 {
                    sriov.vlan_id_buf.push(c);
                }
            }
            KeyCode::Backspace => {
                sriov.vlan_id_buf.pop();
            }
            KeyCode::Enter => {
                let vf_idx = sriov.vf_table_cursor as u32;
                let buf = sriov.vlan_id_buf.clone();
                let vlan_id: u16 = buf.parse().unwrap_or(0);

                if let Some(ref mut profile) = sriov.editing_profile {
                    let existing = profile.vf.iter_mut().find(|o| o.index == vf_idx);
                    if vlan_id == 0 {
                        if let Some(o) = existing {
                            o.vlan = None;
                        }
                    } else {
                        let vlan = crate::config::VlanConfig {
                            id: vlan_id,
                            qos: None,
                            proto: None,
                        };
                        if let Some(o) = existing {
                            o.vlan = Some(vlan);
                        } else {
                            profile.vf.push(crate::config::VfOverride {
                                index: vf_idx,
                                mac: None,
                                trust: None,
                                spoofchk: None,
                                vlan: Some(vlan),
                            });
                            profile.vf.sort_by_key(|o| o.index);
                        }
                    }
                }
                sriov.edit_focus = None;
            }
            KeyCode::Esc => {
                sriov.edit_focus = None;
            }
            _ => {}
        }
        return;
    }

    let num_vfs = sriov
        .editing_profile
        .as_ref()
        .map(|p| p.num_vfs.min(128) as usize)
        .unwrap_or(0);

    if num_vfs == 0 {
        return;
    }

    let cursor = &mut sriov.vf_table_cursor;
    let scroll = &mut sriov.vf_table_scroll;

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if *cursor > 0 {
                *cursor -= 1;
            } else {
                *cursor = num_vfs - 1;
            }
            adjust_scroll(*cursor, scroll);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            *cursor = (*cursor + 1) % num_vfs;
            adjust_scroll(*cursor, scroll);
        }
        KeyCode::Char('t') => {
            toggle_vf_field(sriov, |o, defaults| {
                let current = o.trust.unwrap_or(defaults.trust);
                o.trust = Some(!current);
            });
        }
        KeyCode::Char('s') if key.modifiers.is_empty() => {
            toggle_vf_field(sriov, |o, defaults| {
                let current = o.spoofchk.unwrap_or(defaults.spoofchk);
                o.spoofchk = Some(!current);
            });
        }
        KeyCode::Char('v') => {
            let vf_idx = sriov.vf_table_cursor as u32;
            let current_vlan = sriov
                .editing_profile
                .as_ref()
                .and_then(|p| p.vf.iter().find(|o| o.index == vf_idx))
                .and_then(|o| o.vlan.as_ref())
                .map(|v| v.id.to_string())
                .unwrap_or_default();
            sriov.vlan_id_buf = current_vlan;
            sriov.edit_focus = Some(EditFocus::VfVlanId);
        }
        KeyCode::Char('d') => {
            let idx = sriov.vf_table_cursor as u32;
            if let Some(ref mut profile) = sriov.editing_profile {
                profile.vf.retain(|o| o.index != idx);
            }
        }
        _ => {}
    }
}

fn adjust_scroll(cursor: usize, scroll: &mut usize) {
    if cursor < *scroll {
        *scroll = cursor;
    } else if cursor >= *scroll + VF_TABLE_VISIBLE_ROWS {
        *scroll = cursor + 1 - VF_TABLE_VISIBLE_ROWS;
    }
}

fn toggle_vf_field(
    sriov: &mut SriovState,
    f: impl FnOnce(&mut crate::config::VfOverride, &crate::config::VfDefaults),
) {
    let vf_idx = sriov.vf_table_cursor as u32;
    if let Some(ref mut profile) = sriov.editing_profile {
        let defaults = profile.defaults.clone();
        let existing = profile.vf.iter_mut().find(|o| o.index == vf_idx);
        if let Some(o) = existing {
            f(o, &defaults);
        } else {
            let mut new_override = crate::config::VfOverride {
                index: vf_idx,
                mac: None,
                trust: None,
                spoofchk: None,
                vlan: None,
            };
            f(&mut new_override, &defaults);
            profile.vf.push(new_override);
            profile.vf.sort_by_key(|o| o.index);
        }
    }
}

fn ensure_editing_profile(sriov: &mut SriovState) {
    if sriov.editing_profile.is_none() {
        sriov.editing_profile = Some(crate::config::SriovProfileConfig {
            mac: perigee_core::mac::MacAddress::ZERO,
            num_vfs: 0,
            mac_strategy: MacStrategyConfig::Sequential,
            defaults: crate::config::VfDefaults::default(),
            vf: Vec::new(),
            fdb: crate::config::FdbConfig::default(),
        });
    }
}
