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
use crate::pf_ring::RingState;

// ── General tab fields ──

const GENERAL_FIELDS: usize = 6;
const FIELD_VF_COUNT: usize = 0;
const FIELD_MAC_STRATEGY: usize = 1;
const FIELD_TRUST: usize = 2;
const FIELD_SPOOFCHK: usize = 3;
const FIELD_LINK_STATE: usize = 4;
const FIELD_RX_RING: usize = 5;
/// Shown on its own row under the field: a long hint would be clipped at 80 columns.
const RING_MAX_UNKNOWN: &str = "Device max unknown";
const PF_NOT_FOUND: &str = "PF not found on this host";
const CHOOSE_PF: &str = "choose a PF on the PF tab";
const MAX_REPORTED_ZERO: &str = "device reports 0";
/// 80 columns - 2 border - 3 indent - "Device max unknown ()" (21).
const MAX_RING_REASON_CHARS: usize = 54;
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
    if let Some(warning) = rx_ring_over_max_warning(
        profile.as_ref().and_then(|p| p.rx_ring),
        &sriov.pf_ring_probe,
    ) {
        lines.push(Line::from(Span::styled(
            format!("   {warning}"),
            common::style_warn(),
        )));
    }
    if cursor == FIELD_RX_RING {
        for text in [
            RX_RING_HINT.to_string(),
            ring_max_text(&sriov.pf_ring_probe, pf_chosen(sriov)),
        ] {
            lines.push(Line::from(Span::styled(
                format!("   {text}"),
                common::style_muted(),
            )));
        }
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

/// The "Device max" row. Rows must fit 80 columns: 2 border + 3 indent leave
/// 75 for this text, so a long failure reason is cut.
fn ring_max_text(probe: &Option<Result<RingState, String>>, pf_chosen: bool) -> String {
    match probe {
        Some(Ok(ring)) => match ring.known_max() {
            Some(max) => format!("Device max {max}, current {}", ring.current),
            None => format!("{RING_MAX_UNKNOWN} ({MAX_REPORTED_ZERO})"),
        },
        Some(Err(reason)) => {
            let reason = crate::pf_ring::truncate_reason(reason, MAX_RING_REASON_CHARS);
            format!("{RING_MAX_UNKNOWN} ({reason})")
        }
        None if pf_chosen => format!("{RING_MAX_UNKNOWN} ({PF_NOT_FOUND})"),
        None => format!("{RING_MAX_UNKNOWN} ({CHOOSE_PF})"),
    }
}

/// A profile with a real MAC has a PF behind it; a new profile's zero MAC
/// (or no profile at all) means none was chosen yet.
fn pf_chosen(sriov: &SriovState) -> bool {
    sriov
        .editing_profile
        .as_ref()
        .is_some_and(|p| p.mac != perigee_core::mac::MacAddress::ZERO)
}

/// Warning shown when a stored `rx_ring` is above a known device maximum
/// (PF changed after typing the value, or a hand-edited file). The value is
/// never changed silently. Shared by the General and Review tabs.
pub(super) fn rx_ring_over_max_warning(
    rx_ring: Option<u32>,
    probe: &Option<Result<RingState, String>>,
) -> Option<String> {
    let n = rx_ring?;
    let max = known_ring_max(probe)?;
    (n > max).then(|| format!("⚠ RX ring {n} exceeds device max {max}; apply will refuse it"))
}

/// The device maximum, when a probe succeeded and the device reported one.
fn known_ring_max(probe: &Option<Result<RingState, String>>) -> Option<u32> {
    match probe {
        Some(Ok(ring)) => ring.known_max(),
        _ => None,
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
    match parse_rx_ring(&sriov.rx_ring_buf, known_ring_max(&sriov.pf_ring_probe)) {
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

/// Empty means unmanaged. With a known device maximum, larger values are
/// refused here; when it is unknown the apply step still checks.
fn parse_rx_ring(text: &str, device_max: Option<u32>) -> Result<Option<u32>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u32>() {
        Ok(0) => Err("RX ring must be greater than 0 (clear the field for unmanaged)".to_string()),
        Ok(n) => match device_max {
            Some(max) if n > max => Err(format!("RX ring {n} exceeds the device maximum {max}")),
            _ => Ok(Some(n)),
        },
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
    use crate::cmd::CmdError;

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

    fn ring(current: u32, max: u32) -> Option<Result<RingState, String>> {
        Some(Ok(RingState { current, max }))
    }

    #[test]
    fn ring_max_row_shows_the_probed_numbers() {
        let mut sriov = state_with(None);
        sriov.pf_ring_probe = ring(1024, 8192);

        let text = general_screen_at(&sriov, 80);

        assert!(text.contains("Device max 8192, current 1024"), "{text}");
        assert!(
            text.contains("Empty = unmanaged"),
            "the hint row stays: {text}"
        );
    }

    #[test]
    fn ring_max_row_says_unknown_when_the_probe_failed() {
        let mut sriov = state_with(None);
        sriov.pf_ring_probe = Some(Err("ethtool not installed".to_string()));

        let text = general_screen_at(&sriov, 80);

        assert!(
            text.contains("Device max unknown (ethtool not installed)"),
            "{text}"
        );
        assert!(text.contains("Empty = unmanaged"), "{text}");
    }

    fn choose_mac(sriov: &mut SriovState) {
        if let Some(profile) = sriov.editing_profile.as_mut() {
            profile.mac = perigee_core::mac::MacAddress([2, 0, 0, 0, 0, 1]);
        }
    }

    #[test]
    fn profile_whose_mac_resolves_to_no_interface_says_pf_not_found() {
        let mut sriov = state_with(None);
        choose_mac(&mut sriov);

        let text = general_screen_at(&sriov, 80);

        assert!(
            text.contains("Device max unknown (PF not found on this host)"),
            "{text}"
        );
    }

    #[test]
    fn new_profile_without_a_pf_says_to_choose_one() {
        let zero_mac = state_with(None);
        let mut no_profile = state_with(None);
        no_profile.editing_profile = None;

        for sriov in [&zero_mac, &no_profile] {
            let text = general_screen_at(sriov, 80);

            assert!(
                text.contains("Device max unknown (choose a PF on the PF tab)"),
                "{text}"
            );
            assert!(!text.contains("PF not found"), "{text}");
        }
    }

    fn reason_row_at_80_columns(error: CmdError) -> String {
        let run = move |_: &str, _: &[&str]| -> Result<String, CmdError> {
            Err(match &error {
                CmdError::Spawn { cmd, .. } => CmdError::Spawn {
                    cmd: cmd.clone(),
                    source: std::io::Error::from_raw_os_error(2),
                },
                CmdError::TimedOut { cmd, timeout } => CmdError::TimedOut {
                    cmd: cmd.clone(),
                    timeout: *timeout,
                },
                other => panic!("unsupported fixture {other:?}"),
            })
        };
        let mut sriov = state_with(None);
        choose_mac(&mut sriov);
        sriov.pf_ring_probe = Some(crate::pf_ring::probe_with("enp65s0np0", &run));
        general_screen_at(&sriov, 80)
    }

    #[test]
    fn missing_ethtool_shows_the_io_error_at_80_columns() {
        let text = reason_row_at_80_columns(CmdError::Spawn {
            cmd: "ethtool -g enp65s0np0".to_string(),
            source: std::io::Error::from_raw_os_error(2),
        });

        assert!(
            text.contains("Device max unknown (cannot run ethtool: No such file"),
            "{text}"
        );
    }

    #[test]
    fn hung_ethtool_shows_timed_out_at_80_columns() {
        let text = reason_row_at_80_columns(CmdError::TimedOut {
            cmd: "ethtool -g enp65s0np0".to_string(),
            timeout: crate::pf_ring::RING_PROBE_TIMEOUT,
        });

        assert!(
            text.contains("Device max unknown (ethtool timed out)"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failing_ethtool_shows_its_stderr_at_80_columns() {
        use std::os::unix::process::ExitStatusExt;
        let run = |_: &str, _: &[&str]| -> Result<String, CmdError> {
            Err(CmdError::Failed {
                cmd: "ethtool -g enp65s0np0".to_string(),
                status: std::process::ExitStatus::from_raw(1 << 8),
                stderr: "netlink error: Operation not supported\n".to_string(),
            })
        };
        let mut sriov = state_with(None);
        choose_mac(&mut sriov);
        sriov.pf_ring_probe = Some(crate::pf_ring::probe_with("enp65s0np0", &run));

        let text = general_screen_at(&sriov, 80);

        assert!(
            text.contains("Device max unknown (netlink error: Operation not supported)"),
            "{text}"
        );
    }

    #[test]
    fn a_reported_maximum_of_zero_is_unknown_and_accepts_any_value() {
        let mut sriov = state_with(None);
        sriov.pf_ring_probe = ring(1024, 0);

        let text = general_screen_at(&sriov, 80);
        commit(&mut sriov, "16384");

        assert!(
            text.contains("Device max unknown (device reports 0)"),
            "{text}"
        );
        assert_eq!(rx_ring(&sriov), Some(16384));
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn warning_appears_only_when_the_value_exceeds_a_known_maximum() {
        let warn =
            |rx, probe: Option<Result<RingState, String>>| rx_ring_over_max_warning(rx, &probe);

        let above = warn(Some(8192), ring(1024, 1024)).unwrap_or_default();
        assert!(above.contains("8192") && above.contains("1024"), "{above}");
        assert!(above.contains("apply will refuse it"), "{above}");
        assert_eq!(warn(Some(1024), ring(1024, 1024)), None, "equal to max");
        assert_eq!(warn(Some(1), ring(1024, 1024)), None, "below max");
        assert_eq!(warn(Some(9000), None), None, "no probe");
        assert_eq!(
            warn(Some(9000), Some(Err("x".to_string()))),
            None,
            "failed probe"
        );
        assert_eq!(warn(Some(9000), ring(1024, 0)), None, "max reported as 0");
        assert_eq!(warn(None, ring(1024, 1024)), None, "unmanaged");
    }

    #[test]
    fn the_warning_fits_80_columns_with_the_widest_numbers() {
        let warning =
            rx_ring_over_max_warning(Some(u32::MAX), &ring(1, u32::MAX - 1)).unwrap_or_default();

        assert!(warning.chars().count() + 3 + 2 <= 80, "{warning}");
    }

    #[test]
    fn general_tab_warns_whatever_the_cursor_position() {
        let mut sriov = state_with(Some(8192));
        sriov.pf_ring_probe = ring(1024, 1024);
        sriov.general_cursor = FIELD_VF_COUNT;

        let text = general_screen_at(&sriov, 80);

        assert!(
            text.contains("RX ring 8192 exceeds device max 1024; apply will refuse it"),
            "{text}"
        );
    }

    #[test]
    fn general_tab_has_no_warning_when_unmanaged_or_within_the_maximum() {
        for rx in [None, Some(1024), Some(512)] {
            let mut sriov = state_with(rx);
            sriov.pf_ring_probe = ring(1024, 1024);

            let text = general_screen_at(&sriov, 80);

            assert!(!text.contains("exceeds device max"), "{rx:?}: {text}");
        }
    }

    #[test]
    fn a_very_long_reason_is_cut_so_the_row_fits_80_columns() {
        let mut sriov = state_with(None);
        sriov.pf_ring_probe = Some(Err("x".repeat(500)));

        let row = ring_max_text(&sriov.pf_ring_probe, true);
        let text = general_screen_at(&sriov, 80);

        assert!(row.starts_with("Device max unknown ("), "{row}");
        assert!(row.ends_with(')'), "{row}");
        // 3 columns of indent plus the two border columns.
        assert!(row.chars().count() + 3 + 2 <= 80, "{row}");
        assert!(text.contains(&row), "the whole row is visible: {text}");
    }

    #[test]
    fn ring_max_row_is_hidden_when_the_cursor_is_elsewhere() {
        let mut sriov = state_with(None);
        sriov.pf_ring_probe = ring(1024, 8192);
        sriov.general_cursor = FIELD_VF_COUNT;

        let text = general_screen_at(&sriov, 80);

        assert!(!text.contains("Device max"), "{text}");
    }

    #[test]
    fn a_value_equal_to_the_known_maximum_is_accepted() {
        let mut sriov = state_with(None);
        sriov.pf_ring_probe = ring(1024, 8192);

        commit(&mut sriov, "8192");

        assert_eq!(rx_ring(&sriov), Some(8192));
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn a_value_above_the_known_maximum_is_rejected_naming_both_numbers() {
        let mut sriov = state_with(Some(1024));
        sriov.pf_ring_probe = ring(1024, 8192);

        commit(&mut sriov, "8193");

        assert_eq!(rx_ring(&sriov), Some(1024));
        assert_eq!(sriov.edit_focus, None);
        let message = sriov.message.unwrap_or_default();
        assert!(
            message.contains("8193") && message.contains("8192"),
            "{message}"
        );
    }

    #[test]
    fn clearing_to_unmanaged_is_accepted_whatever_the_maximum() {
        let mut sriov = state_with(Some(1024));
        sriov.pf_ring_probe = ring(1024, 8192);

        commit(&mut sriov, "");

        assert_eq!(rx_ring(&sriov), None);
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn an_unknown_maximum_accepts_any_value() {
        for probe in [None, Some(Err("no ethtool".to_string()))] {
            let mut sriov = state_with(None);
            sriov.pf_ring_probe = probe;

            commit(&mut sriov, "16384");

            assert_eq!(rx_ring(&sriov), Some(16384));
            assert_eq!(sriov.message, None);
        }
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
