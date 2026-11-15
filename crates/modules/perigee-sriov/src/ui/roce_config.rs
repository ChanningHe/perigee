use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::vf_config::{ensure_editing_profile, styled_field_line, value_style};
use super::{EditFocus, SriovState};
use crate::config::RoceConfig;

pub const PFC_FLAP_WARNING: &str =
    "⚠ PFC changes flap the PF link: with VFs present they wait for next boot";

/// Row 0 is the Enabled toggle; the numeric rows follow in `NUMERIC_ROWS` order.
const ROW_ENABLED: usize = 0;
const ROW_COUNT: usize = NUMERIC_ROWS.len() + 1;
/// Wide enough for the longest label, so values line up in one column.
const LABEL_WIDTH: usize = 24;
/// Row prefix + label + longest value + this must stay within 80 columns.
const ENABLED_HINT: &str = "Enter to toggle; creates defaults";
const FIX_FIRST_HINT: &str = "fix the value first";
const ENABLE_FIRST_MESSAGE: &str = "Enable RoCE first (select Enabled and press Enter)";

/// One numeric RoCE field: how to show it and how to build a changed copy.
/// `with` only narrows the value to the field type; range rules live in
/// `RoceConfig::validate`.
struct NumericRow {
    label: &'static str,
    get: fn(&RoceConfig) -> u64,
    with: fn(&RoceConfig, u64) -> Result<RoceConfig, String>,
}

fn narrow<T: TryFrom<u64>>(value: u64) -> Result<T, String> {
    T::try_from(value).map_err(|_| format!("{value} is too large for this field"))
}

const NUMERIC_ROWS: [NumericRow; 5] = [
    NumericRow {
        label: "DSCP",
        get: |c| u64::from(c.dscp),
        with: |c, v| {
            Ok(RoceConfig {
                dscp: narrow(v)?,
                ..c.clone()
            })
        },
    },
    NumericRow {
        label: "Priority (PFC)",
        get: |c| u64::from(c.priority),
        with: |c, v| {
            Ok(RoceConfig {
                priority: narrow(v)?,
                ..c.clone()
            })
        },
    },
    NumericRow {
        label: "CNP DSCP",
        get: |c| u64::from(c.cnp_dscp),
        with: |c, v| {
            Ok(RoceConfig {
                cnp_dscp: narrow(v)?,
                ..c.clone()
            })
        },
    },
    NumericRow {
        label: "CNP priority",
        get: |c| u64::from(c.cnp_priority),
        with: |c, v| {
            Ok(RoceConfig {
                cnp_priority: narrow(v)?,
                ..c.clone()
            })
        },
    },
    NumericRow {
        label: "PFC storm timeout (ms)",
        get: |c| u64::from(c.pfc_storm_timeout_ms),
        with: |c, v| {
            Ok(RoceConfig {
                pfc_storm_timeout_ms: narrow(v)?,
                ..c.clone()
            })
        },
    },
];

fn numeric_row(cursor: usize) -> Option<&'static NumericRow> {
    cursor.checked_sub(1).and_then(|i| NUMERIC_ROWS.get(i))
}

fn current_roce(sriov: &SriovState) -> Option<&RoceConfig> {
    sriov.editing_profile.as_ref().and_then(|p| p.roce.as_ref())
}

// ── Rendering ──

pub fn render(frame: &mut Frame, sriov: &SriovState, area: Rect) {
    let roce = current_roce(sriov);
    let mut lines = vec![Line::from(""), enabled_line(sriov, roce)];
    for (i, row) in NUMERIC_ROWS.iter().enumerate() {
        lines.push(numeric_line(sriov, roce, i + 1, row));
    }
    lines.extend(footer_lines(roce));

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, area);
}

fn label_cell(label: &str) -> String {
    format!("{:<width$}", label, width = LABEL_WIDTH)
}

fn enabled_line(sriov: &SriovState, roce: Option<&RoceConfig>) -> Line<'static> {
    let is_active = sriov.roce_cursor == ROW_ENABLED;
    let value = match roce {
        None => "not configured",
        Some(r) if r.enabled => "ON",
        Some(_) => "OFF",
    };
    styled_field_line(
        is_active,
        &label_cell("Enabled"),
        value,
        value_style(false, is_active),
        ENABLED_HINT,
    )
}

fn numeric_line(
    sriov: &SriovState,
    roce: Option<&RoceConfig>,
    row_index: usize,
    row: &NumericRow,
) -> Line<'static> {
    let is_active = sriov.roce_cursor == row_index;
    let is_editing = is_active && sriov.edit_focus == Some(EditFocus::RoceField);
    let (value, style, hint) = match roce {
        _ if is_editing => (
            format!("[{}▎]", sriov.roce_buf),
            value_style(true, true),
            "",
        ),
        Some(r) => (
            (row.get)(r).to_string(),
            value_style(false, is_active),
            "Enter to edit",
        ),
        None => (
            (row.get)(&RoceConfig::default()).to_string(),
            common::style_muted(),
            "Enable RoCE first",
        ),
    };
    styled_field_line(is_active, &label_cell(row.label), &value, style, hint)
}

/// Derived ToS, the PFC warning (only when RoCE is on) and the switch reminder.
fn footer_lines(roce: Option<&RoceConfig>) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    if let Some(r) = roce {
        let tos = r
            .roce_tos()
            .map_or_else(|| "invalid".to_string(), |t| t.to_string());
        lines.push(Line::from(Span::styled(
            format!("   default_roce_tos: {tos}"),
            common::style_value(),
        )));
        if r.enabled {
            lines.push(Line::from(Span::styled(
                format!("   {PFC_FLAP_WARNING}"),
                common::style_warn(),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        "   These values must match the switch configuration.",
        common::style_muted(),
    )));
    lines
}

// ── Input ──

pub fn handle_input(sriov: &mut SriovState, key: KeyEvent) {
    if sriov.edit_focus == Some(EditFocus::RoceField) {
        handle_edit(sriov, key);
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            sriov.roce_cursor = common::wrap_step(sriov.roce_cursor, ROW_COUNT, false);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            sriov.roce_cursor = common::wrap_step(sriov.roce_cursor, ROW_COUNT, true);
        }
        KeyCode::Enter | KeyCode::Char(' ') => activate(sriov),
        _ => {}
    }
}

fn activate(sriov: &mut SriovState) {
    if sriov.roce_cursor == ROW_ENABLED {
        toggle_enabled(sriov);
    } else {
        start_edit(sriov);
    }
}

/// Create the section enabled with defaults, or flip `enabled` keeping the
/// numbers. The TUI never removes the section. Turning it on is validated so a
/// hand-edited invalid section cannot be enabled; turning it off never is.
fn toggle_enabled(sriov: &mut SriovState) {
    ensure_editing_profile(sriov);
    let Some(profile) = sriov.editing_profile.as_mut() else {
        return;
    };
    let next = match profile.roce.as_ref() {
        None => RoceConfig {
            enabled: true,
            ..RoceConfig::default()
        },
        Some(r) => RoceConfig {
            enabled: !r.enabled,
            ..r.clone()
        },
    };
    if next.enabled {
        if let Err(e) = next.validate() {
            sriov.message = Some(format!("{e}; {FIX_FIRST_HINT}"));
            return;
        }
    }
    profile.roce = Some(next);
    sriov.message = None;
}

fn start_edit(sriov: &mut SriovState) {
    let Some(row) = numeric_row(sriov.roce_cursor) else {
        return;
    };
    let Some(roce) = current_roce(sriov) else {
        sriov.message = Some(ENABLE_FIRST_MESSAGE.to_string());
        return;
    };
    let prefill = (row.get)(roce).to_string();
    sriov.roce_buf = prefill;
    sriov.edit_focus = Some(EditFocus::RoceField);
    sriov.message = None;
}

fn handle_edit(sriov: &mut SriovState, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) if c.is_ascii_digit() => sriov.roce_buf.push(c),
        KeyCode::Backspace => {
            sriov.roce_buf.pop();
        }
        KeyCode::Enter => commit(sriov),
        KeyCode::Esc => {
            sriov.roce_buf.clear();
            sriov.message = None;
            sriov.edit_focus = None;
        }
        _ => {}
    }
}

/// Store the validated candidate, or leave the config alone and report why.
/// The edit ends either way.
fn commit(sriov: &mut SriovState) {
    sriov.edit_focus = None;
    match candidate_from_buffer(sriov) {
        Ok(candidate) => {
            if let Some(profile) = sriov.editing_profile.as_mut() {
                profile.roce = Some(candidate);
            }
            sriov.message = None;
        }
        Err(msg) => sriov.message = Some(msg),
    }
    sriov.roce_buf.clear();
}

fn candidate_from_buffer(sriov: &SriovState) -> Result<RoceConfig, String> {
    let row = numeric_row(sriov.roce_cursor).ok_or("No RoCE field selected")?;
    let base = current_roce(sriov).ok_or("RoCE section is missing")?;
    let value: u64 = sriov
        .roce_buf
        .parse()
        .map_err(|_| format!("{}: enter a whole number that fits the field", row.label))?;
    let candidate = (row.with)(base, value).map_err(|e| format!("{}: {e}", row.label))?;
    candidate.validate().map_err(|e| e.to_string())?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RoceConfig, SriovFileConfig};
    use crate::ui::vf_config::{self, ensure_editing_profile};
    use crate::ui::EditFocus;
    use crossterm::event::KeyCode;
    use ratatui::{backend::TestBackend, Terminal};

    const ENABLED: usize = 0;
    const DSCP: usize = 1;
    const PRIORITY: usize = 2;
    const CNP_DSCP: usize = 3;
    const CNP_PRIORITY: usize = 4;
    const STORM_TIMEOUT: usize = 5;

    fn press(sriov: &mut SriovState, code: KeyCode) {
        handle_input(sriov, KeyEvent::from(code));
    }

    fn type_text(sriov: &mut SriovState, text: &str) {
        for c in text.chars() {
            press(sriov, KeyCode::Char(c));
        }
    }

    /// State holding a fresh profile with the given RoCE section.
    fn state_with(roce: Option<RoceConfig>) -> SriovState {
        let mut sriov = SriovState::new();
        ensure_editing_profile(&mut sriov);
        if let Some(profile) = sriov.editing_profile.as_mut() {
            profile.roce = roce;
        }
        sriov
    }

    fn enabled_defaults() -> RoceConfig {
        RoceConfig {
            enabled: true,
            ..RoceConfig::default()
        }
    }

    fn roce_of(sriov: &SriovState) -> Option<RoceConfig> {
        sriov.editing_profile.as_ref().and_then(|p| p.roce.clone())
    }

    /// Select `row`, start an edit, replace the buffer with `text`, press Enter.
    fn commit(sriov: &mut SriovState, row: usize, text: &str) {
        sriov.roce_cursor = row;
        press(sriov, KeyCode::Enter);
        while !sriov.roce_buf.is_empty() {
            press(sriov, KeyCode::Backspace);
        }
        type_text(sriov, text);
        press(sriov, KeyCode::Enter);
    }

    fn toggle(sriov: &mut SriovState) {
        sriov.roce_cursor = ENABLED;
        press(sriov, KeyCode::Enter);
    }

    fn screen_text(sriov: &SriovState) -> String {
        screen_text_at(sriov, 110)
    }

    fn screen_text_at(sriov: &SriovState, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, sriov, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── cursor ──

    #[test]
    fn cursor_wraps_in_both_directions() {
        let mut sriov = state_with(None);

        press(&mut sriov, KeyCode::Up);
        assert_eq!(sriov.roce_cursor, STORM_TIMEOUT);
        press(&mut sriov, KeyCode::Down);
        assert_eq!(sriov.roce_cursor, ENABLED);
        press(&mut sriov, KeyCode::Char('j'));
        assert_eq!(sriov.roce_cursor, DSCP);
        press(&mut sriov, KeyCode::Char('k'));
        assert_eq!(sriov.roce_cursor, ENABLED);
    }

    // ── enabled toggle ──

    #[test]
    fn toggle_from_none_creates_enabled_defaults() {
        let mut sriov = state_with(None);

        toggle(&mut sriov);

        assert_eq!(roce_of(&sriov), Some(enabled_defaults()));
        assert_eq!(sriov.edit_focus, None);
    }

    #[test]
    fn toggling_again_disables_and_keeps_the_numbers_then_reenables() {
        let mut sriov = state_with(None);
        toggle(&mut sriov);
        commit(&mut sriov, DSCP, "30");
        let tuned = RoceConfig {
            dscp: 30,
            ..enabled_defaults()
        };
        assert_eq!(roce_of(&sriov), Some(tuned.clone()));

        toggle(&mut sriov);
        assert_eq!(
            roce_of(&sriov),
            Some(RoceConfig {
                enabled: false,
                ..tuned.clone()
            })
        );

        toggle(&mut sriov);
        assert_eq!(roce_of(&sriov), Some(tuned));
    }

    #[test]
    fn space_toggles_like_enter() {
        let mut sriov = state_with(None);

        press(&mut sriov, KeyCode::Char(' '));

        assert_eq!(roce_of(&sriov), Some(enabled_defaults()));
    }

    #[test]
    fn toggle_keeps_the_section_even_when_disabled() {
        let mut sriov = state_with(Some(enabled_defaults()));

        toggle(&mut sriov);

        assert!(roce_of(&sriov).is_some_and(|r| !r.enabled));
    }

    #[test]
    fn toggle_refuses_to_enable_a_section_with_invalid_numbers() {
        let invalid_disabled = [
            RoceConfig {
                dscp: 30,
                cnp_dscp: 30,
                ..RoceConfig::default()
            },
            RoceConfig {
                dscp: 99,
                ..RoceConfig::default()
            },
        ];
        for stored in invalid_disabled {
            let mut sriov = state_with(Some(stored.clone()));

            toggle(&mut sriov);

            assert_eq!(roce_of(&sriov), Some(stored.clone()), "{stored:?}");
            let message = sriov.message.unwrap_or_default();
            assert!(message.contains("fix the value first"), "{message}");
        }
    }

    #[test]
    fn toggle_enables_a_valid_disabled_section() {
        let mut sriov = state_with(Some(RoceConfig {
            dscp: 24,
            ..RoceConfig::default()
        }));

        toggle(&mut sriov);

        assert_eq!(
            roce_of(&sriov),
            Some(RoceConfig {
                enabled: true,
                dscp: 24,
                ..RoceConfig::default()
            })
        );
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn invalid_enabled_section_can_still_be_turned_off() {
        let stored = RoceConfig {
            enabled: true,
            dscp: 99,
            ..RoceConfig::default()
        };
        let mut sriov = state_with(Some(stored.clone()));

        toggle(&mut sriov);

        assert_eq!(
            roce_of(&sriov),
            Some(RoceConfig {
                enabled: false,
                ..stored
            })
        );
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn escape_clears_a_message_left_over_from_an_earlier_action() {
        let mut sriov = state_with(Some(enabled_defaults()));
        sriov.roce_cursor = DSCP;
        press(&mut sriov, KeyCode::Enter);
        sriov.message = Some("stale".to_string());

        press(&mut sriov, KeyCode::Esc);

        assert_eq!(sriov.message, None);
        assert_eq!(roce_of(&sriov), Some(enabled_defaults()));
    }

    // ── numeric rows ──

    #[test]
    fn numeric_edit_starts_prefilled_with_the_current_value() {
        let mut sriov = state_with(Some(enabled_defaults()));
        sriov.roce_cursor = STORM_TIMEOUT;

        press(&mut sriov, KeyCode::Enter);

        assert_eq!(sriov.edit_focus, Some(EditFocus::RoceField));
        assert_eq!(sriov.roce_buf, "100");
    }

    #[test]
    fn valid_edits_are_stored_for_every_numeric_row() {
        let cases: [(usize, &str, RoceConfig); 5] = [
            (
                DSCP,
                "24",
                RoceConfig {
                    dscp: 24,
                    ..enabled_defaults()
                },
            ),
            (
                PRIORITY,
                "5",
                RoceConfig {
                    priority: 5,
                    ..enabled_defaults()
                },
            ),
            (
                CNP_DSCP,
                "46",
                RoceConfig {
                    cnp_dscp: 46,
                    ..enabled_defaults()
                },
            ),
            (
                CNP_PRIORITY,
                "7",
                RoceConfig {
                    cnp_priority: 7,
                    ..enabled_defaults()
                },
            ),
            (
                STORM_TIMEOUT,
                "8000",
                RoceConfig {
                    pfc_storm_timeout_ms: 8000,
                    ..enabled_defaults()
                },
            ),
        ];
        for (row, text, expected) in cases {
            let mut sriov = state_with(Some(enabled_defaults()));

            commit(&mut sriov, row, text);

            assert_eq!(roce_of(&sriov), Some(expected), "row {row} = {text}");
            assert_eq!(sriov.edit_focus, None);
            assert_eq!(sriov.message, None);
        }
    }

    #[test]
    fn invalid_values_are_rejected_with_the_stored_config_unchanged() {
        let cases = [
            (DSCP, "64"),
            (PRIORITY, "8"),
            (CNP_DSCP, "64"),
            (CNP_PRIORITY, "8"),
            (CNP_DSCP, "26"),
            (STORM_TIMEOUT, "50"),
            (STORM_TIMEOUT, "8001"),
            (DSCP, "256"),
            (STORM_TIMEOUT, "65536"),
            (DSCP, "99999999999999999999999"),
            (DSCP, ""),
        ];
        for (row, text) in cases {
            let mut sriov = state_with(Some(enabled_defaults()));

            commit(&mut sriov, row, text);

            assert_eq!(
                roce_of(&sriov),
                Some(enabled_defaults()),
                "row {row} = {text:?}"
            );
            assert_eq!(sriov.edit_focus, None, "row {row} = {text:?}");
            assert!(sriov.message.is_some(), "row {row} = {text:?}");
        }
    }

    #[test]
    fn dscp_equal_to_the_cnp_dscp_is_rejected() {
        let mut sriov = state_with(Some(enabled_defaults()));

        commit(&mut sriov, DSCP, "48");

        assert_eq!(roce_of(&sriov), Some(enabled_defaults()));
        assert!(sriov.message.is_some());
    }

    #[test]
    fn storm_timeout_zero_is_accepted() {
        let mut sriov = state_with(Some(enabled_defaults()));

        commit(&mut sriov, STORM_TIMEOUT, "0");

        assert_eq!(
            roce_of(&sriov).map(|r| r.pfc_storm_timeout_ms),
            Some(0),
            "0 disables the storm timeout"
        );
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn a_later_valid_edit_clears_the_previous_error() {
        let mut sriov = state_with(Some(enabled_defaults()));
        commit(&mut sriov, DSCP, "64");
        assert!(sriov.message.is_some());

        commit(&mut sriov, DSCP, "40");

        assert_eq!(roce_of(&sriov).map(|r| r.dscp), Some(40));
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn escape_cancels_without_changing_the_config() {
        let mut sriov = state_with(Some(enabled_defaults()));
        sriov.roce_cursor = DSCP;

        press(&mut sriov, KeyCode::Enter);
        type_text(&mut sriov, "1");
        press(&mut sriov, KeyCode::Esc);

        assert_eq!(roce_of(&sriov), Some(enabled_defaults()));
        assert_eq!(sriov.edit_focus, None);
        assert_eq!(sriov.message, None);
    }

    #[test]
    fn non_digit_keys_are_ignored_while_editing() {
        let mut sriov = state_with(Some(enabled_defaults()));
        sriov.roce_cursor = DSCP;
        press(&mut sriov, KeyCode::Enter);
        while !sriov.roce_buf.is_empty() {
            press(&mut sriov, KeyCode::Backspace);
        }

        type_text(&mut sriov, "2x-4 .");
        press(&mut sriov, KeyCode::Tab);

        assert_eq!(sriov.roce_buf, "24");
        assert_eq!(sriov.edit_focus, Some(EditFocus::RoceField));
    }

    #[test]
    fn numeric_row_without_a_section_does_not_edit_or_create_it() {
        for row in [DSCP, PRIORITY, CNP_DSCP, CNP_PRIORITY, STORM_TIMEOUT] {
            let mut sriov = state_with(None);
            sriov.roce_cursor = row;

            press(&mut sriov, KeyCode::Enter);

            assert_eq!(sriov.edit_focus, None, "row {row}");
            assert_eq!(roce_of(&sriov), None, "row {row}");
            assert!(
                sriov
                    .message
                    .as_deref()
                    .is_some_and(|m| m.contains("Enable RoCE first")),
                "row {row}: {:?}",
                sriov.message
            );
        }
    }

    // ── rendering ──

    #[test]
    fn render_shows_not_configured_and_dimmed_defaults_without_a_section() {
        let sriov = state_with(None);

        let text = screen_text(&sriov);

        assert!(text.contains("not configured"), "{text}");
        assert!(text.contains("PFC storm timeout (ms)"), "{text}");
        assert!(!text.contains("default_roce_tos"), "{text}");
        assert!(!text.contains("PFC changes flap"), "{text}");
    }

    #[test]
    fn render_shows_tos_and_pfc_warning_when_enabled() {
        let sriov = state_with(Some(enabled_defaults()));

        let text = screen_text(&sriov);

        assert!(text.contains("ON"), "{text}");
        assert!(text.contains("default_roce_tos: 106"), "{text}");
        assert!(text.contains("PFC changes flap the PF link"), "{text}");
        assert!(text.contains("switch"), "{text}");
    }

    #[test]
    fn render_hides_the_pfc_warning_when_disabled_but_keeps_the_tos() {
        let sriov = state_with(Some(RoceConfig::default()));

        let text = screen_text(&sriov);

        assert!(text.contains("OFF"), "{text}");
        assert!(text.contains("default_roce_tos: 106"), "{text}");
        assert!(!text.contains("PFC changes flap"), "{text}");
    }

    #[test]
    fn rows_and_texts_fit_80_columns_without_a_section() {
        let sriov = state_with(None);

        let text = screen_text_at(&sriov, 80);

        assert!(text.contains("not configured"), "{text}");
        assert!(text.contains("toggle"), "{text}");
        assert!(text.contains("defaults"), "{text}");
        assert!(text.contains("must match the switch"), "{text}");
    }

    #[test]
    fn warning_and_tos_fit_80_columns_when_enabled() {
        let sriov = state_with(Some(enabled_defaults()));

        let text = screen_text_at(&sriov, 80);

        assert!(text.contains("default_roce_tos: 106"), "{text}");
        assert!(text.contains(PFC_FLAP_WARNING), "{text}");
        assert!(text.contains("must match the switch"), "{text}");
    }

    #[test]
    fn render_shows_the_edit_buffer_while_editing() {
        let mut sriov = state_with(Some(enabled_defaults()));
        sriov.roce_cursor = PRIORITY;
        press(&mut sriov, KeyCode::Enter);

        let text = screen_text(&sriov);

        assert!(text.contains("[3▎]"), "{text}");
    }

    // ── persistence ──

    fn temp_path(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("perigee-roce-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sriov.toml");
        (dir, path)
    }

    const LEGACY: &str = "[sriov.p]\nmac = \"8c:2a:8e:88:9a:f6\"\nnum_vfs = 4\n\n[sriov.p.defaults]\ntrust = true\nspoofchk = false\n";

    #[test]
    fn profile_edited_through_the_new_controls_round_trips() {
        let (dir, path) = temp_path("roundtrip");
        std::fs::write(&path, LEGACY).unwrap();
        let mut sriov = SriovState::new();
        sriov.editing_name = "p".to_string();
        sriov.editing_profile = Some(SriovFileConfig::load(&path).unwrap().sriov["p"].clone());

        sriov.general_cursor = 5;
        vf_config::handle_general_input(&mut sriov, KeyEvent::from(KeyCode::Enter));
        for c in "8192".chars() {
            vf_config::handle_general_input(&mut sriov, KeyEvent::from(KeyCode::Char(c)));
        }
        vf_config::handle_general_input(&mut sriov, KeyEvent::from(KeyCode::Enter));
        toggle(&mut sriov);
        commit(&mut sriov, DSCP, "24");
        commit(&mut sriov, STORM_TIMEOUT, "0");
        sriov.save_config_to(&path).unwrap();

        let saved = SriovFileConfig::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let profile = &saved.sriov["p"];
        assert_eq!(profile.rx_ring, Some(8192));
        assert_eq!(
            profile.roce,
            Some(RoceConfig {
                dscp: 24,
                pfc_storm_timeout_ms: 0,
                ..enabled_defaults()
            })
        );
    }

    #[test]
    fn unrelated_edit_on_a_legacy_file_adds_no_roce_or_rx_ring_keys() {
        let (dir, path) = temp_path("legacy");
        std::fs::write(&path, LEGACY).unwrap();
        let mut sriov = SriovState::new();
        sriov.editing_name = "p".to_string();
        sriov.editing_profile = Some(SriovFileConfig::load(&path).unwrap().sriov["p"].clone());

        // Browse the new controls without committing anything.
        sriov.roce_cursor = DSCP;
        press(&mut sriov, KeyCode::Enter);
        sriov.general_cursor = 5;
        vf_config::handle_general_input(&mut sriov, KeyEvent::from(KeyCode::Enter));
        vf_config::handle_general_input(&mut sriov, KeyEvent::from(KeyCode::Esc));
        sriov.general_cursor = 2;
        vf_config::handle_general_input(&mut sriov, KeyEvent::from(KeyCode::Enter));
        sriov.save_config_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("trust = false"), "the edit was saved: {text}");
        for key in ["roce", "rx_ring", "link_state"] {
            assert!(!text.contains(key), "{key} leaked into: {text}");
        }
    }
}
