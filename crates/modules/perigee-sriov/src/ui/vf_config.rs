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

const GENERAL_FIELDS: usize = 6;
const FIELD_VF_COUNT: usize = 0;
const FIELD_MAC_STRATEGY: usize = 1;
const FIELD_TRUST: usize = 2;
const FIELD_SPOOFCHK: usize = 3;
const FIELD_LINK_STATE: usize = 4;
const FIELD_RX_RING: usize = 5;
/// Shown on its own row under the field: a long hint would be clipped at 80 columns.
const RX_RING_HINT: &str = "Empty = unmanaged. A live change briefly drops the PF link.";

/// Value style shared by the field rows: editing beats selected beats plain.
pub(super) fn value_style(is_editing: bool, is_active: bool) -> Style {
    if is_editing {
        common::style_editing()
    } else if is_active {
        common::style_selected()
    } else {
        common::style_value()
    }
}

/// One `label  value  hint` row of a field list; the hint shows on the active row.
pub(super) fn styled_field_line(
    is_active: bool,
    label: &str,
    value: &str,
    val_style: Style,
    hint: &str,
) -> Line<'static> {
    let indicator = if is_active { " ▸ " } else { "   " };
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
}

pub fn render_general(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let profile = &sriov.editing_profile;
    let cursor = sriov.general_cursor;
    let is_editing_vf_count = sriov.edit_focus == Some(EditFocus::GeneralVfCount);

    let vf_count_display = if is_editing_vf_count {
        format!("[{}▎]", sriov.vf_count_buf)
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
    let link_state = profile
        .as_ref()
        .and_then(|p| p.defaults.link_state)
        .map_or("unmanaged", |s| s.as_str());

    let is_editing_rx_ring = sriov.edit_focus == Some(EditFocus::GeneralRxRing);
    let rx_ring_display = if is_editing_rx_ring {
        format!("[{}▎]", sriov.rx_ring_buf)
    } else {
        profile
            .as_ref()
            .and_then(|p| p.rx_ring)
            .map_or_else(|| "unmanaged".to_string(), |n| n.to_string())
    };

    let field_line =
        |idx: usize, label: &str, value: &str, is_editing: bool, hint: &str| -> Line<'static> {
            let is_active = cursor == idx;
            let val_style = value_style(is_editing, is_active);
            styled_field_line(is_active, label, value, val_style, hint)
        };

    let mut lines = vec![
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
        Line::from(""),
        field_line(
            FIELD_LINK_STATE,
            "Link State:",
            link_state,
            false,
            "Enter to cycle (unmanaged leaves VFs as they are)",
        ),
        Line::from(""),
        field_line(
            FIELD_RX_RING,
            "PF RX ring:",
            &rx_ring_display,
            is_editing_rx_ring,
            "",
        ),
    ];
    if cursor == FIELD_RX_RING {
        lines.push(Line::from(Span::styled(
            format!("   {RX_RING_HINT}"),
            common::style_muted(),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

pub fn handle_general_input(sriov: &mut SriovState, key: KeyEvent) {
    if sriov.edit_focus == Some(EditFocus::GeneralRxRing) {
        handle_rx_ring_edit(sriov, key);
        return;
    }

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
                FIELD_LINK_STATE => {
                    if let Some(ref mut profile) = sriov.editing_profile {
                        profile.defaults.link_state =
                            crate::config::next_link_state(profile.defaults.link_state);
                    }
                }
                FIELD_RX_RING => start_rx_ring_edit(sriov),
                _ => {}
            }
        }
        _ => {}
    }
}

fn start_rx_ring_edit(sriov: &mut SriovState) {
    sriov.rx_ring_buf = sriov
        .editing_profile
        .as_ref()
        .and_then(|p| p.rx_ring)
        .map(|n| n.to_string())
        .unwrap_or_default();
    sriov.message = None;
    sriov.edit_focus = Some(EditFocus::GeneralRxRing);
}

fn handle_rx_ring_edit(sriov: &mut SriovState, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) if c.is_ascii_digit() => sriov.rx_ring_buf.push(c),
        KeyCode::Backspace => {
            sriov.rx_ring_buf.pop();
        }
        KeyCode::Enter => commit_rx_ring(sriov),
        KeyCode::Esc => {
            sriov.rx_ring_buf.clear();
            sriov.message = None;
            sriov.edit_focus = None;
        }
        _ => {}
    }
}

/// Apply the edit buffer. A rejected value leaves the profile untouched and
/// explains why in the message line; the edit ends either way.
fn commit_rx_ring(sriov: &mut SriovState) {
    sriov.edit_focus = None;
    match parse_rx_ring(&sriov.rx_ring_buf) {
        Ok(value) => {
            if let Some(profile) = sriov.editing_profile.as_mut() {
                profile.rx_ring = value;
            }
            sriov.message = None;
        }
        Err(msg) => sriov.message = Some(msg),
    }
    sriov.rx_ring_buf.clear();
}

/// Empty means unmanaged. The device maximum is checked at apply time, not here.
fn parse_rx_ring(text: &str) -> Result<Option<u32>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u32>() {
        Ok(0) => Err("RX ring must be greater than 0 (clear the field for unmanaged)".to_string()),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(format!("RX ring must be at most {}", u32::MAX)),
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
            "   {:>3}  {:<14} {:<18} {:<6} {:<9} {:<8} {:<6} {:<10} {}",
            "VF#", "PCI Addr", "MAC", "Trust", "SpoofChk", "Link", "VLAN", "Used By", "Override"
        ),
        common::style_muted(),
    )));
    lines.push(Line::from(Span::styled(
        format!("   {}", "─".repeat(93)),
        Style::default().fg(common::BORDER),
    )));

    if let Some(profile) = profile {
        let num = profile.num_vfs.min(128) as usize;
        let cursor = sriov.vf_table_cursor;
        let scroll = sriov.vf_table_scroll;
        let visible_end = (scroll + VF_TABLE_VISIBLE_ROWS).min(num);

        // PF iface + VM usage are cached on editor open (see refresh_vf_usage)
        // so this render path does no per-frame filesystem scanning. VFs only
        // have a PCI address once created/applied; before that (and if the PF
        // can't be located) every row shows "-".
        let pf_iface = sriov.editor_pf_iface.as_deref();
        let vf_users = &sriov.vf_users;

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
            let link_val = vf_override
                .and_then(|o| o.link_state)
                .or(profile.defaults.link_state)
                .map_or("-", |s| s.as_str());
            let pci = pf_iface.and_then(|pf| perigee_core::sysfs::read_vf_pci_addr(pf, i as u32));
            let pci_str = pci.as_deref().unwrap_or("-").to_string();
            // "Used By" colored span: green if the referencing VM is running.
            let (used_text, used_color) = match pci
                .as_deref()
                .and_then(|p| vf_users.get(&crate::vm_usage::normalize_pci(p)))
            {
                Some(u) if u.running => (format!("VM {}", u.vmid), common::SUCCESS),
                Some(u) => (format!("VM {}", u.vmid), common::TEXT_MUTED),
                None => ("-".to_string(), common::TEXT_MUTED),
            };
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
                format!("[{}▎]", sriov.vlan_id_buf)
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
                        "{}{:>3}  {:<14} {:<18} {:<6} {:<9} {:<8} {:<6} ",
                        indicator,
                        i,
                        pci_str,
                        mac_str,
                        if trust_val { "✓" } else { "✗" },
                        if spoof_val { "✓" } else { "✗" },
                        link_val,
                        vlan_str,
                    ),
                    style,
                ),
                Span::styled(
                    format!("{:<10}", used_text),
                    Style::default().fg(used_color),
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
                    " l ",
                    Style::default()
                        .fg(common::KEY_FG)
                        .bg(common::KEY_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" link  ", common::style_muted()),
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
            // No other arm takes Char, so a 5th digit still falls to `_`.
            KeyCode::Char(c) if c.is_ascii_digit() && sriov.vlan_id_buf.len() < 4 => {
                sriov.vlan_id_buf.push(c);
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
                                link_state: None,
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
        KeyCode::Char('l') => {
            // Cycles the override itself; unset inherits the profile default.
            toggle_vf_field(sriov, |o, _| {
                o.link_state = crate::config::next_link_state(o.link_state);
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
                link_state: None,
            };
            f(&mut new_override, &defaults);
            profile.vf.push(new_override);
            profile.vf.sort_by_key(|o| o.index);
        }
    }
}

pub(super) fn ensure_editing_profile(sriov: &mut SriovState) {
    if sriov.editing_profile.is_none() {
        sriov.editing_profile = Some(crate::config::SriovProfileConfig {
            mac: perigee_core::mac::MacAddress::ZERO,
            num_vfs: 0,
            mac_strategy: MacStrategyConfig::Sequential,
            defaults: crate::config::VfDefaults::default(),
            vf: Vec::new(),
            fdb: crate::config::FdbConfig::default(),
            roce: None,
            rx_ring: None,
        });
    }
}

#[cfg(test)]
mod rx_ring_tests {
    use super::*;

    fn press(sriov: &mut SriovState, code: KeyCode) {
        handle_general_input(sriov, KeyEvent::from(code));
    }

    fn type_text(sriov: &mut SriovState, text: &str) {
        for c in text.chars() {
            press(sriov, KeyCode::Char(c));
        }
    }

    /// State on the RX ring row with a profile holding `rx_ring`.
    fn state_with(rx_ring: Option<u32>) -> SriovState {
        let mut sriov = SriovState::new();
        ensure_editing_profile(&mut sriov);
        if let Some(profile) = sriov.editing_profile.as_mut() {
            profile.rx_ring = rx_ring;
        }
        sriov.general_cursor = FIELD_RX_RING;
        sriov
    }

    fn rx_ring(sriov: &SriovState) -> Option<u32> {
        sriov.editing_profile.as_ref().and_then(|p| p.rx_ring)
    }

    /// Start an edit, replace the buffer with `text`, and press Enter.
    fn commit(sriov: &mut SriovState, text: &str) {
        press(sriov, KeyCode::Enter);
        while !sriov.rx_ring_buf.is_empty() {
            press(sriov, KeyCode::Backspace);
        }
        type_text(sriov, text);
        press(sriov, KeyCode::Enter);
    }

    #[test]
    fn rx_ring_is_the_sixth_general_field_after_link_state() {
        assert_eq!(GENERAL_FIELDS, 6);
        assert_eq!(FIELD_RX_RING, FIELD_LINK_STATE + 1);
    }

    #[test]
    fn rx_ring_cursor_wraps_through_the_new_field() {
        let mut sriov = state_with(None);
        sriov.general_cursor = FIELD_LINK_STATE;

        press(&mut sriov, KeyCode::Down);
        assert_eq!(sriov.general_cursor, FIELD_RX_RING);
        press(&mut sriov, KeyCode::Down);
        assert_eq!(sriov.general_cursor, FIELD_VF_COUNT);
        press(&mut sriov, KeyCode::Up);
        assert_eq!(sriov.general_cursor, FIELD_RX_RING);
    }

    #[test]
    fn enter_starts_an_edit_prefilled_with_the_current_value() {
        let mut sriov = state_with(Some(4096));

        press(&mut sriov, KeyCode::Enter);

        assert_eq!(sriov.edit_focus, Some(EditFocus::GeneralRxRing));
        assert_eq!(sriov.rx_ring_buf, "4096");
    }

    #[test]
    fn enter_on_unmanaged_starts_an_edit_with_an_empty_buffer() {
        let mut sriov = state_with(None);
        sriov.rx_ring_buf = "stale".to_string();

        press(&mut sriov, KeyCode::Enter);

        assert_eq!(sriov.edit_focus, Some(EditFocus::GeneralRxRing));
        assert!(sriov.rx_ring_buf.is_empty());
    }

    #[test]
    fn typing_a_number_and_enter_sets_the_ring_size() {
        let mut sriov = state_with(None);

        commit(&mut sriov, "8192");

        assert_eq!(rx_ring(&sriov), Some(8192));
        assert_eq!(sriov.edit_focus, None);
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn clearing_the_buffer_and_enter_makes_it_unmanaged() {
        let mut sriov = state_with(Some(8192));

        commit(&mut sriov, "");

        assert_eq!(rx_ring(&sriov), None);
        assert_eq!(sriov.edit_focus, None);
    }

    #[test]
    fn zero_is_rejected_and_keeps_the_value() {
        let mut sriov = state_with(Some(4096));

        commit(&mut sriov, "0");

        assert_eq!(rx_ring(&sriov), Some(4096));
        assert_eq!(sriov.edit_focus, None);
        assert!(sriov.message.is_some());
    }

    #[test]
    fn a_value_overflowing_u32_is_rejected_and_keeps_the_value() {
        let mut sriov = state_with(Some(4096));

        commit(&mut sriov, "4294967296");

        assert_eq!(rx_ring(&sriov), Some(4096));
        assert_eq!(sriov.edit_focus, None);
        assert!(sriov.message.is_some());
    }

    #[test]
    fn u32_max_is_accepted() {
        let mut sriov = state_with(None);

        commit(&mut sriov, "4294967295");

        assert_eq!(rx_ring(&sriov), Some(u32::MAX));
    }

    #[test]
    fn escape_cancels_without_changing_the_value() {
        let mut sriov = state_with(Some(4096));

        press(&mut sriov, KeyCode::Enter);
        type_text(&mut sriov, "99");
        press(&mut sriov, KeyCode::Esc);

        assert_eq!(rx_ring(&sriov), Some(4096));
        assert_eq!(sriov.edit_focus, None);
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn non_digit_keys_are_ignored_while_editing() {
        let mut sriov = state_with(None);

        press(&mut sriov, KeyCode::Enter);
        type_text(&mut sriov, "1a-2 .3e");
        press(&mut sriov, KeyCode::Tab);

        assert_eq!(sriov.rx_ring_buf, "123");
        assert_eq!(sriov.edit_focus, Some(EditFocus::GeneralRxRing));
    }

    fn general_screen(sriov: &SriovState) -> String {
        general_screen_at(sriov, 140)
    }

    fn general_screen_at(sriov: &SriovState, width: u16) -> String {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24))
            .expect("test terminal");
        terminal
            .draw(|frame| render_general(frame, sriov, frame.area()))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn render_shows_unmanaged_the_number_and_the_edit_buffer() {
        let mut sriov = state_with(None);
        let before = general_screen(&sriov);
        let ring_row = before
            .lines()
            .find(|l| l.contains("PF RX ring:"))
            .unwrap_or_default();
        assert!(
            ring_row.contains("PF RX ring:") && ring_row.contains("unmanaged"),
            "{before}"
        );

        sriov.editing_profile.as_mut().expect("profile").rx_ring = Some(4096);
        let text = general_screen(&sriov);
        let ring_row = text.lines().find(|l| l.contains("PF RX ring:"));
        assert!(ring_row.is_some_and(|l| l.contains("4096")), "{text}");
        assert!(text.contains("briefly drops the PF link"), "{text}");

        press(&mut sriov, KeyCode::Enter);
        type_text(&mut sriov, "1");
        assert!(general_screen(&sriov).contains("[40961▎]"));
    }

    #[test]
    fn rx_ring_row_and_hint_fit_80_columns() {
        let mut sriov = state_with(None);
        press(&mut sriov, KeyCode::Enter);
        type_text(&mut sriov, "4294967295");

        let text = general_screen_at(&sriov, 80);

        let ring_row = text.lines().find(|l| l.contains("PF RX ring:"));
        assert!(
            ring_row.is_some_and(|l| l.contains("[4294967295▎]")),
            "{text}"
        );
        assert!(text.contains("Empty = unmanaged"), "{text}");
        assert!(text.contains("briefly drops the PF link"), "{text}");
    }

    #[test]
    fn starting_an_edit_clears_a_stale_message() {
        let mut sriov = state_with(Some(4096));
        sriov.message = Some("stale".to_string());

        press(&mut sriov, KeyCode::Enter);

        assert_eq!(sriov.message, None);
    }

    #[test]
    fn escape_clears_a_message_left_over_from_an_earlier_action() {
        let mut sriov = state_with(Some(4096));
        press(&mut sriov, KeyCode::Enter);
        sriov.message = Some("stale".to_string());

        press(&mut sriov, KeyCode::Esc);

        assert_eq!(sriov.message, None);
        assert_eq!(rx_ring(&sriov), Some(4096));
    }

    #[test]
    fn backspace_on_an_empty_buffer_is_harmless() {
        let mut sriov = state_with(None);

        press(&mut sriov, KeyCode::Enter);
        press(&mut sriov, KeyCode::Backspace);

        assert!(sriov.rx_ring_buf.is_empty());
        assert_eq!(sriov.edit_focus, Some(EditFocus::GeneralRxRing));
    }
}
