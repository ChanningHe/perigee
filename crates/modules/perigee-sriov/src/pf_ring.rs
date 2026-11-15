//! Opt-in PF RX ring size (`rx_ring`), applied with `ethtool -G`.

use tracing::info;

use std::time::Duration;

use crate::cmd::{CmdError, CMD_TIMEOUT};
use crate::roce::StepError;

/// Runs one external command; injectable for tests.
type Runner<'a> = &'a dyn Fn(&str, &[&str]) -> Result<String, CmdError>;

/// Limit for the read-only probe the TUI runs on the UI thread; the apply
/// path's `CMD_TIMEOUT` would freeze the screen for too long.
pub const RING_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// RX ring sizes reported by `ethtool -g`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingState {
    pub current: u32,
    pub max: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RingPlan {
    Keep,
    Write(u32),
    Refuse(String),
}

#[derive(Debug, Default)]
pub struct RingOutcome {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// An `ethtool` command timed out: the RoCE commands are skipped too.
    pub timed_out: bool,
}

#[derive(Clone, Copy)]
enum Section {
    Other,
    Max,
    Current,
}

/// Current RX ring size and its pre-set maximum. Both sections label the
/// value `RX:`; `RX Mini:` and `RX Jumbo:` are different fields.
pub fn parse_ring(out: &str) -> Result<RingState, String> {
    let (mut max, mut current) = (None, None);
    let mut section = Section::Other;
    for line in out.lines() {
        match line.trim() {
            "Pre-set maximums:" => section = Section::Max,
            "Current hardware settings:" => section = Section::Current,
            l => {
                let Some(("RX", value)) = l.split_once(':').map(|(k, v)| (k.trim(), v.trim()))
                else {
                    continue;
                };
                let slot = match section {
                    Section::Max => &mut max,
                    Section::Current => &mut current,
                    Section::Other => continue,
                };
                let n = value
                    .parse::<u32>()
                    .map_err(|_| format!("unexpected RX ring value '{value}'"))?;
                slot.get_or_insert(n);
            }
        }
    }
    match (current, max) {
        (Some(current), Some(max)) => Ok(RingState { current, max }),
        _ => Err("no RX ring size in `ethtool -g` output".to_string()),
    }
}

/// Write only a value that differs and that the device accepts.
pub fn plan(want: u32, state: RingState) -> RingPlan {
    if state.current == want {
        RingPlan::Keep
    } else if want > state.max {
        RingPlan::Refuse(format!(
            "rx_ring {want} exceeds the device maximum {}; ring left at {}",
            state.max, state.current
        ))
    } else {
        RingPlan::Write(want)
    }
}

/// Bring the PF's RX ring to `want`; `None` (key absent) runs no command.
/// `live`: VFs already exist, so the brief queue rebuild is worth a warning.
pub fn apply_rx_ring(pf: &str, want: Option<u32>, live: bool) -> RingOutcome {
    apply_with(pf, want, live, &|program, args| {
        crate::cmd::run(program, args, CMD_TIMEOUT)
    })
}

fn apply_with(pf: &str, want: Option<u32>, live: bool, run: Runner) -> RingOutcome {
    let mut out = RingOutcome::default();
    match want {
        None => {}
        Some(0) => out.fail("must be > 0, ring left unchanged".into(), None),
        Some(n) => match read(pf, run) {
            Err(e) => out.fail(format!("cannot read the RX ring of {pf}"), Some(e)),
            Ok(state) => match plan(n, state) {
                RingPlan::Keep => {}
                RingPlan::Refuse(e) => out.fail(e, None),
                RingPlan::Write(n) => write_and_check(pf, n, live, run, &mut out),
            },
        },
    }
    out
}

fn write_and_check(pf: &str, n: u32, live: bool, run: Runner, out: &mut RingOutcome) {
    if let Err(e) = run("ethtool", &["-G", pf, "rx", &n.to_string()]) {
        let ctx = format!("setting the RX ring of {pf} to {n} failed (ring state unknown)");
        return out.fail(ctx, Some(e.into()));
    }
    info!(pf = %pf, rx_ring = n, "PF RX ring set");
    if live {
        out.warnings.push(format!(
            "rx_ring: {pf} RX ring changed to {n} while VFs exist; the PF's queues were \
             rebuilt and traffic on the PF netdev (host and bridged guests) was interrupted \
             for about a second"
        ));
    }
    match read(pf, run) {
        Ok(s) if s.current != n => out.warnings.push(format!(
            "rx_ring: {pf} reports an RX ring of {} after writing {n} (driver rounding); set \
             rx_ring = {} or every apply rebuilds the PF's queues again",
            s.current, s.current
        )),
        Ok(_) => {}
        Err(e) => out.fail(format!("cannot read back the RX ring of {pf}"), Some(e)),
    }
}

/// Read the PF's ring sizes with `ethtool -g` only (never writes).
pub fn probe(pf: &str) -> Result<RingState, String> {
    probe_with(pf, &|program, args| {
        crate::cmd::run(program, args, RING_PROBE_TIMEOUT)
    })
}

const MAX_PROBE_REASON_CHARS: usize = 80;

/// Cut `text` to at most `max` characters, ending in `…` when shortened.
pub fn truncate_reason(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let cut: String = text.chars().take(keep).collect();
    if max == 0 {
        cut
    } else {
        cut + "…"
    }
}

/// Like `read`, but errors are a short cause for the TUI: no command line
/// (it only repeats `ethtool -g <pf>`), so what is left is the actual reason.
pub(crate) fn probe_with(pf: &str, run: Runner) -> Result<RingState, String> {
    let out = run("ethtool", &["-g", pf]).map_err(|e| one_line(&short_cause(&e)))?;
    parse_ring(&out).map_err(|e| one_line(&e))
}

fn short_cause(e: &CmdError) -> String {
    match e {
        CmdError::Spawn { source, .. } => format!("cannot run ethtool: {source}"),
        CmdError::Io { source, .. } => format!("ethtool I/O error: {source}"),
        CmdError::Failed { status, stderr, .. } if stderr.trim().is_empty() => {
            format!("ethtool exited ({status})")
        }
        CmdError::Failed { stderr, .. } => stderr.clone(),
        CmdError::TimedOut { .. } | CmdError::PipesOpen { .. } => "ethtool timed out".to_string(),
    }
}

/// Collapse whitespace (stderr can span lines) and cap the length.
fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_reason(&joined, MAX_PROBE_REASON_CHARS)
}

fn read(pf: &str, run: Runner) -> Result<RingState, StepError> {
    let out = run("ethtool", &["-g", pf])?;
    Ok(parse_ring(&out)?)
}

impl RingState {
    /// The maximum as a usable limit: a device reporting 0 offers nothing to
    /// compare against, so it counts as unknown.
    pub fn known_max(&self) -> Option<u32> {
        (self.max > 0).then_some(self.max)
    }
}

impl RingOutcome {
    fn fail(&mut self, context: String, cause: Option<StepError>) {
        self.timed_out |= cause.as_ref().is_some_and(StepError::is_timeout);
        self.errors.push(match cause {
            Some(e) => format!("rx_ring: {context}: {e}"),
            None => format!("rx_ring: {context}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const PF: &str = "enp65s0np0";

    /// `ethtool -g` on serverhub (PVE 9, ConnectX-5).
    const ETHTOOL_G: &str = "\
Ring parameters for enp65s0np0:
Pre-set maximums:
RX:\t\t\t8192
RX Mini:\t\tn/a
RX Jumbo:\t\tn/a
TX:\t\t\t8192
TX push buff len:\tn/a
Current hardware settings:
RX:\t\t\t1024
RX Mini:\t\tn/a
RX Jumbo:\t\tn/a
TX:\t\t\t1024
RX Buf Len:\t\tn/a
CQE Size:\t\tn/a
TX Push:\t\toff
RX Push:\t\toff
TX push buff len:\tn/a
TCP data split:\t\tn/a
";

    fn ring_output(current: u32, max: u32) -> String {
        ETHTOOL_G
            .replacen("RX:\t\t\t8192", &format!("RX:\t\t\t{max}"), 1)
            .replacen("RX:\t\t\t1024", &format!("RX:\t\t\t{current}"), 1)
    }

    #[test]
    fn probe_returns_the_ring_state_from_ethtool_g() {
        let run = |_: &str, _: &[&str]| Ok(ETHTOOL_G.to_string());

        let state = probe_with(PF, &run);

        assert_eq!(
            state,
            Ok(RingState {
                current: 1024,
                max: 8192
            })
        );
    }

    #[test]
    fn probe_runs_exactly_ethtool_dash_g_on_the_pf() {
        let calls = RefCell::new(Vec::<(String, Vec<String>)>::new());
        let run = |program: &str, args: &[&str]| {
            calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            ));
            Ok(ETHTOOL_G.to_string())
        };

        let _ = probe_with(PF, &run);

        assert_eq!(
            calls.into_inner(),
            vec![(
                "ethtool".to_string(),
                vec!["-g".to_string(), PF.to_string()]
            )]
        );
    }

    #[test]
    fn probe_error_is_short_and_non_empty() {
        let spawn_failure = |_: &str, _: &[&str]| -> Result<String, CmdError> {
            Err(CmdError::Spawn {
                cmd: "ethtool -g x".to_string(),
                source: std::io::Error::other("y".repeat(500)),
            })
        };
        let timeout = |_: &str, _: &[&str]| -> Result<String, CmdError> {
            Err(CmdError::TimedOut {
                cmd: "ethtool -g x".to_string(),
                timeout: RING_PROBE_TIMEOUT,
            })
        };

        for reason in [
            probe_with(PF, &spawn_failure).unwrap_err(),
            probe_with(PF, &timeout).unwrap_err(),
        ] {
            assert!(!reason.is_empty());
            assert!(reason.chars().count() <= MAX_PROBE_REASON_CHARS, "{reason}");
        }
    }

    #[test]
    fn probe_reports_unparseable_output_with_the_parser_message() {
        let output = "Ring parameters for x:\n";
        let run = |_: &str, _: &[&str]| Ok(output.to_string());

        assert_eq!(probe_with(PF, &run), parse_ring(output));
        assert!(probe_with(PF, &run).is_err_and(|e| !e.is_empty()));
    }

    fn failing(error: fn() -> CmdError) -> impl Fn(&str, &[&str]) -> Result<String, CmdError> {
        move |_, _| Err(error())
    }

    fn spawn_error() -> CmdError {
        CmdError::Spawn {
            cmd: "ethtool -g enp65s0np0".to_string(),
            source: std::io::Error::from_raw_os_error(2),
        }
    }

    #[test]
    fn probe_spawn_failure_names_the_io_error_not_the_command_line() {
        let reason = probe_with(PF, &failing(spawn_error)).unwrap_err();

        assert!(reason.starts_with("cannot run ethtool: "), "{reason}");
        assert!(reason.contains("No such file"), "{reason}");
        assert!(!reason.contains("-g"), "{reason}");
    }

    #[test]
    fn probe_io_error_names_the_io_error() {
        let io = || CmdError::Io {
            cmd: "ethtool -g enp65s0np0".to_string(),
            source: std::io::Error::other("broken pipe"),
        };

        let reason = probe_with(PF, &failing(io)).unwrap_err();

        assert!(reason.contains("broken pipe"), "{reason}");
        assert!(!reason.contains("-g"), "{reason}");
    }

    #[test]
    fn probe_timeout_says_ethtool_timed_out() {
        let timeout = || CmdError::TimedOut {
            cmd: "ethtool -g enp65s0np0".to_string(),
            timeout: RING_PROBE_TIMEOUT,
        };

        let reason = probe_with(PF, &failing(timeout)).unwrap_err();

        assert_eq!(reason, "ethtool timed out");
    }

    #[cfg(unix)]
    fn failed(stderr: &str) -> CmdError {
        use std::os::unix::process::ExitStatusExt;
        CmdError::Failed {
            cmd: "ethtool -g enp65s0np0".to_string(),
            status: std::process::ExitStatus::from_raw(1 << 8),
            stderr: stderr.to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn probe_failed_exit_reports_stderr_on_one_line() {
        let run = |_: &str, _: &[&str]| -> Result<String, CmdError> {
            Err(failed("netlink error: Operation not supported\n"))
        };

        let reason = probe_with(PF, &run).unwrap_err();

        assert_eq!(reason, "netlink error: Operation not supported");
    }

    #[cfg(unix)]
    #[test]
    fn probe_failed_exit_with_empty_stderr_reports_the_status() {
        let run = |_: &str, _: &[&str]| -> Result<String, CmdError> { Err(failed("  \n")) };

        let reason = probe_with(PF, &run).unwrap_err();

        assert!(reason.starts_with("ethtool exited"), "{reason}");
        assert!(reason.contains('1'), "{reason}");
    }

    #[test]
    fn known_max_treats_zero_as_unknown() {
        let ring = |max| RingState { current: 1, max };

        assert_eq!(ring(8192).known_max(), Some(8192));
        assert_eq!(ring(1).known_max(), Some(1));
        assert_eq!(ring(0).known_max(), None);
    }

    #[test]
    fn truncate_reason_counts_characters_not_bytes() {
        let cut = truncate_reason("网卡不支持环形缓冲区查询", 5);

        assert_eq!(cut, "网卡不支…");
        assert_eq!(truncate_reason("网卡不支持", 5), "网卡不支持");
        assert_eq!(truncate_reason("……………", 3), "………");
        assert_eq!(truncate_reason("a…b…c…d", 4), "a…b…");
    }

    #[test]
    fn truncate_reason_handles_empty_text_and_zero_width() {
        assert_eq!(truncate_reason("", 5), "");
        assert_eq!(truncate_reason("", 0), "");
        assert_eq!(truncate_reason("abc", 0), "");
        assert_eq!(truncate_reason("abc", 1), "…");
        assert_eq!(truncate_reason("a", 1), "a");
    }

    #[test]
    fn truncate_reason_keeps_short_text_and_marks_cut_text() {
        assert_eq!(truncate_reason("short", 10), "short");
        let cut = truncate_reason("abcdefghij", 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'), "{cut}");
    }

    #[test]
    fn parses_real_ethtool_g_output() {
        let s = parse_ring(ETHTOOL_G).unwrap();
        assert_eq!(
            s,
            RingState {
                current: 1024,
                max: 8192
            }
        );
    }

    #[test]
    fn rx_mini_and_jumbo_are_not_rx() {
        // Only the sub-labels carry numbers: still no RX value anywhere.
        let out = "Pre-set maximums:\nRX Mini:\t4096\nRX Jumbo:\t4096\n\
                   Current hardware settings:\nRX Mini:\t512\nRX Jumbo:\t512\n";
        assert!(parse_ring(out).is_err());
    }

    #[test]
    fn unparseable_or_na_rx_is_an_error() {
        assert!(parse_ring(&ETHTOOL_G.replacen("RX:\t\t\t1024", "RX:\t\t\tn/a", 1)).is_err());
        assert!(parse_ring(&ETHTOOL_G.replacen("RX:\t\t\t8192", "RX:\t\t\tlots", 1)).is_err());
        assert!(parse_ring("Ring parameters for x:\n").is_err());
    }

    #[test]
    fn plan_writes_only_a_valid_difference() {
        let s = RingState {
            current: 1024,
            max: 8192,
        };
        assert_eq!(plan(1024, s), RingPlan::Keep);
        assert_eq!(plan(8192, s), RingPlan::Write(8192));
        match plan(16384, s) {
            RingPlan::Refuse(e) => assert!(e.contains("16384") && e.contains("8192"), "{e}"),
            other => panic!("{other:?}"),
        }
    }

    /// Fake `ethtool`: `-g` answers from `rings` in turn (the last one
    /// repeats); commands starting with `hang` time out. Records every call.
    fn fake<'a>(
        calls: &'a RefCell<Vec<String>>,
        rings: &'a [String],
        hang: &'a str,
    ) -> impl Fn(&str, &[&str]) -> Result<String, CmdError> + 'a {
        move |program, args| {
            let cmd = format!("{program} {}", args.join(" "));
            let reads = calls.borrow().iter().filter(|c| c.contains(" -g ")).count();
            calls.borrow_mut().push(cmd.clone());
            if !hang.is_empty() && cmd.starts_with(hang) {
                return Err(CmdError::TimedOut {
                    cmd,
                    timeout: CMD_TIMEOUT,
                });
            }
            Ok(match args.first() {
                Some(&"-g") => rings[reads.min(rings.len() - 1)].clone(),
                _ => String::new(),
            })
        }
    }

    fn apply(
        want: Option<u32>,
        live: bool,
        rings: &[String],
        hang: &str,
    ) -> (RingOutcome, Vec<String>) {
        let calls = RefCell::new(Vec::new());
        let out = apply_with(PF, want, live, &fake(&calls, rings, hang));
        (out, calls.into_inner())
    }

    #[test]
    fn absent_key_runs_no_command() {
        let (out, calls) = apply(None, true, &[ring_output(1024, 8192)], "");
        assert!(calls.is_empty(), "{calls:?}");
        assert!(out.errors.is_empty() && out.warnings.is_empty());
    }

    #[test]
    fn zero_is_rejected_without_a_command() {
        let (out, calls) = apply(Some(0), false, &[ring_output(1024, 8192)], "");
        assert!(calls.is_empty());
        assert!(out.errors[0].contains("> 0"));
    }

    #[test]
    fn matching_ring_is_not_written() {
        let (out, calls) = apply(Some(1024), true, &[ring_output(1024, 8192)], "");
        assert_eq!(calls, ["ethtool -g enp65s0np0"]);
        assert!(out.errors.is_empty() && out.warnings.is_empty());
    }

    #[test]
    fn above_device_maximum_is_refused_without_a_write() {
        let (out, calls) = apply(Some(16384), false, &[ring_output(1024, 8192)], "");
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(out.errors[0].contains("16384") && out.errors[0].contains("8192"));
    }

    #[test]
    fn unreadable_ring_is_not_written() {
        let na = ETHTOOL_G.replacen("RX:\t\t\t1024", "RX:\t\t\tn/a", 1);
        let (out, calls) = apply(Some(8192), false, &[na], "");
        assert_eq!(calls.len(), 1);
        assert_eq!(out.errors.len(), 1);
    }

    #[test]
    fn boot_write_is_read_back_without_warnings() {
        let rings = [ring_output(1024, 8192), ring_output(8192, 8192)];
        let (out, calls) = apply(Some(8192), false, &rings, "");
        assert_eq!(
            calls,
            [
                "ethtool -g enp65s0np0",
                "ethtool -G enp65s0np0 rx 8192",
                "ethtool -g enp65s0np0"
            ]
        );
        assert!(out.errors.is_empty() && out.warnings.is_empty(), "{out:?}");
    }

    #[test]
    fn live_write_warns_about_the_interruption() {
        let rings = [ring_output(1024, 8192), ring_output(8192, 8192)];
        let (out, _) = apply(Some(8192), true, &rings, "");
        assert_eq!(out.warnings.len(), 1);
        assert!(
            out.warnings[0].contains("interrupted"),
            "{}",
            out.warnings[0]
        );
    }

    #[test]
    fn rounded_read_back_warns_with_the_reported_value() {
        let rings = [ring_output(1024, 8192), ring_output(4096, 8192)];
        let (out, _) = apply(Some(3000), false, &rings, "");
        assert!(out.errors.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(
            out.warnings[0].contains("rx_ring = 4096"),
            "{}",
            out.warnings[0]
        );
    }

    #[test]
    fn timeouts_are_errors_and_flagged() {
        for hang in ["ethtool -g", "ethtool -G"] {
            let (out, calls) = apply(Some(8192), false, &[ring_output(1024, 8192)], hang);
            assert!(out.timed_out, "{hang}");
            assert_eq!(out.errors.len(), 1, "{hang}: {:?}", out.errors);
            assert_eq!(calls.last().map(|c| c.starts_with(hang)), Some(true));
        }
        let (out, _) = apply(Some(8192), false, &[ring_output(1024, 8192)], "");
        assert!(!out.timed_out);
    }
}
