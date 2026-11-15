//! Opt-in PF lossless-Ethernet setup for RoCE (mlx5 only).
//!
//! Pure parts (state parsers, diff planning, step execution policy, command
//! building) are separate from the thin I/O layer in [`apply_pf_qos`] so they
//! are testable without hardware.

mod parse;
pub mod retry;
pub mod tos;

use std::cell::Cell;
use tracing::{info, warn};

use crate::cmd::{CmdError, CMD_TIMEOUT};
use crate::config::RoceConfig;
pub use parse::{parse_dcb_app_json, parse_dcb_pfc_json, parse_pause, parse_tos, parse_tunable};
use tos::{PendingTos, TosAttempt, TosPaths};

const NUM_PRIORITIES: usize = 8;
const MLX5_DRIVER: &str = "mlx5_core";
const PFC_STORM_TUNABLE: &str = "pfc-prevention-tout";

/// 802.3x global pause state as reported by `ethtool -a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pause {
    pub rx: bool,
    pub tx: bool,
}

pub const PAUSE_OFF: Pause = Pause {
    rx: false,
    tx: false,
};

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

impl std::fmt::Display for Pause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rx {} tx {}", on_off(self.rx), on_off(self.tx))
    }
}

/// Observed PF state. `Err` holds why a value could not be read; the matching
/// step is then skipped rather than written blind.
#[derive(Debug, Clone)]
pub struct PfQosState {
    pub dscp_prio: Result<Vec<(u8, u8)>, String>,
    pub pfc: Result<[bool; NUM_PRIORITIES], String>,
    pub pause: Result<Pause, String>,
    pub storm_timeout_ms: Result<u16, String>,
}

/// One command needed to reach the configured state. The RoCE default ToS
/// is a configfs write handled separately (see [`tos`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QosStep {
    /// DSCP -> priority entries missing from the app table.
    DscpPrio(Vec<(u8, u8)>),
    SetPause(Pause),
    /// Enable PFC on exactly this priority.
    Pfc(u8),
    StormTimeout(u16),
}

/// Why a step failed. A timeout is kept apart because the tool or driver is
/// hung, so no further commands are issued (see [`execute_plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    Failed(String),
    TimedOut(String),
}

impl StepError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, StepError::TimedOut(_))
    }
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::Failed(m) | StepError::TimedOut(m) => f.write_str(m),
        }
    }
}

impl From<String> for StepError {
    fn from(m: String) -> Self {
        StepError::Failed(m)
    }
}

impl From<CmdError> for StepError {
    fn from(e: CmdError) -> Self {
        if e.is_timeout() {
            StepError::TimedOut(e.to_string())
        } else {
            StepError::Failed(e.to_string())
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QosPlan {
    pub steps: Vec<QosStep>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

// ── Planning ──

fn desired_pfc(priority: u8) -> [bool; NUM_PRIORITIES] {
    std::array::from_fn(|i| i == priority as usize)
}

#[derive(PartialEq)]
enum PfcNeed {
    Matches,
    Change,
    Unknown,
}

/// Decide which writes bring the PF from `state` to `cfg`. Nothing that
/// already matches is written; nothing that could not be read is written.
///
/// Step order: DSCP map, pause, PFC, storm timeout. Pause goes off
/// before PFC goes on so the two are never active together, and only when
/// PFC is (or is about to be) on, so the port never ends up with neither.
///
/// `existing_vfs` is the PF's current `sriov_numvfs` (`None` if unreadable).
/// The PFC change flaps the PF link on mlx5, so it is only made while no VFs
/// exist (at boot); otherwise PFC and pause-off are deferred with an error.
pub fn plan(cfg: &RoceConfig, state: &PfQosState, existing_vfs: Option<u32>) -> QosPlan {
    let mut p = QosPlan::default();
    let live = existing_vfs != Some(0);
    let vfs_desc = match existing_vfs {
        Some(n) => format!("{n} VFs exist"),
        None => "the VF count could not be read".to_string(),
    };

    match &state.dscp_prio {
        Ok(current) => {
            let missing: Vec<(u8, u8)> =
                [(cfg.dscp, cfg.priority), (cfg.cnp_dscp, cfg.cnp_priority)]
                    .into_iter()
                    .filter(|e| !current.contains(e))
                    .collect();
            if !missing.is_empty() {
                if current.is_empty() && live {
                    p.warnings.push(format!(
                        "first DSCP mapping switches the PF to DSCP trust while {vfs_desc}; \
                         this may briefly reset the PF's queues"
                    ));
                }
                p.steps.push(QosStep::DscpPrio(missing));
            }
        }
        Err(e) => p.errors.push(format!("DSCP map not applied: {e}")),
    }

    let pfc = match &state.pfc {
        Ok(current) if *current == desired_pfc(cfg.priority) => PfcNeed::Matches,
        Ok(_) => PfcNeed::Change,
        Err(e) => {
            p.errors.push(format!(
                "PFC and global pause not changed: cannot read PFC: {e}"
            ));
            PfcNeed::Unknown
        }
    };
    let pfc_deferred = pfc == PfcNeed::Change && live;
    if pfc_deferred {
        p.errors.push(format!(
            "PFC not applied: {vfs_desc} and the PFC change flaps the PF link; it is applied \
             when the profile is applied with no VFs present (next boot). Global pause is left \
             unchanged until then"
        ));
    }
    let pfc_will_be_on = pfc == PfcNeed::Matches || (pfc == PfcNeed::Change && !pfc_deferred);

    let pause_known = match &state.pause {
        Ok(pause) => {
            if *pause != PAUSE_OFF && pfc_will_be_on {
                p.steps.push(QosStep::SetPause(PAUSE_OFF));
            }
            true
        }
        Err(e) => {
            p.errors.push(format!("global pause not changed: {e}"));
            false
        }
    };

    if pfc == PfcNeed::Change && !pfc_deferred {
        if pause_known {
            p.steps.push(QosStep::Pfc(cfg.priority));
        } else {
            p.errors.push(
                "PFC not applied: global pause state unknown and PFC must not be combined with it"
                    .to_string(),
            );
        }
    }

    match &state.storm_timeout_ms {
        Ok(t) if *t == cfg.pfc_storm_timeout_ms => {}
        Ok(_) => p
            .steps
            .push(QosStep::StormTimeout(cfg.pfc_storm_timeout_ms)),
        Err(e) => p.errors.push(format!("PFC storm timeout not applied: {e}")),
    }

    p
}

// ── Execution policy ──

/// Set pause to `target`. `ethtool -A` exits non-zero when it considers the
/// request a no-op, so a failure is re-checked against the re-read state
/// (not after a timeout: the tool is hung and the re-read would hang too).
fn set_pause(
    target: Pause,
    exec: &mut impl FnMut(&QosStep) -> Result<(), StepError>,
    read_pause: &mut impl FnMut() -> Result<Pause, StepError>,
) -> Result<(), StepError> {
    match exec(&QosStep::SetPause(target)) {
        Err(e) if !e.is_timeout() => match read_pause() {
            Ok(now) if now == target => Ok(()),
            Err(r) if r.is_timeout() => Err(StepError::TimedOut(format!(
                "{e}; re-reading global pause: {r}"
            ))),
            _ => Err(e),
        },
        res => res,
    }
}

/// After a failed PFC write with global pause already off, put the original
/// pause back. After a PFC timeout this is the single command still issued,
/// without a re-read.
fn restore_pause(
    e: StepError,
    orig: Pause,
    exec: &mut impl FnMut(&QosStep) -> Result<(), StepError>,
    read_pause: &mut impl FnMut() -> Result<Pause, StepError>,
) -> StepError {
    let restored = if e.is_timeout() {
        exec(&QosStep::SetPause(orig))
    } else {
        set_pause(orig, exec, read_pause)
    };
    let msg = match &restored {
        Ok(()) => format!("{e}; global pause restored to {orig}"),
        Err(e2) if e.is_timeout() => format!(
            "{e}; restoring global pause ({orig}) also failed: {e2}. The port may have NO flow \
             control (PFC state unknown, global pause off)"
        ),
        Err(e2) => format!(
            "{e}; restoring global pause ({orig}) also failed: {e2}. The port currently has NO \
             flow control (neither PFC nor global pause)"
        ),
    };
    if e.is_timeout() || restored.is_err_and(|r| r.is_timeout()) {
        StepError::TimedOut(msg)
    } else {
        StepError::Failed(msg)
    }
}

/// A timed-out pause change leaves global pause unknown; whether the port may
/// then have no flow control depends on PFC.
fn pause_unknown(e: StepError, pfc_on: bool) -> StepError {
    match e {
        StepError::TimedOut(m) if pfc_on => {
            StepError::TimedOut(format!("{m}. Global pause state unknown; PFC is on"))
        }
        StepError::TimedOut(m) => StepError::TimedOut(format!(
            "{m}. Global pause state unknown and PFC not enabled: the port may have NO flow \
             control"
        )),
        other => other,
    }
}

/// Run the plan's steps through `exec`, returning the errors. A failing step
/// does not stop the independent ones after it, except that PFC is skipped
/// when pause could not be turned off, and a failed PFC write restores the
/// original pause setting so the port is not left without flow control.
///
/// A timed-out step stops the plan: the tool or driver is hung and every
/// further command would cost another [`CMD_TIMEOUT`]. The pause restore
/// after a failed PFC write is still attempted, once.
pub fn execute_plan(
    plan: &QosPlan,
    original_pause: Option<Pause>,
    mut exec: impl FnMut(&QosStep) -> Result<(), StepError>,
    mut read_pause: impl FnMut() -> Result<Pause, StepError>,
) -> Vec<String> {
    let mut errors = Vec::new();
    // None: no pause step in the plan; Some(ok): whether it succeeded.
    let mut pause_changed: Option<bool> = None;
    // Pause is only turned off when PFC is on or about to be: no PFC step
    // means PFC is already on.
    let pfc_on = !plan.steps.iter().any(|s| matches!(s, QosStep::Pfc(_)));
    for (i, step) in plan.steps.iter().enumerate() {
        let res = match step {
            QosStep::SetPause(target) => {
                let res = set_pause(*target, &mut exec, &mut read_pause);
                pause_changed = Some(res.is_ok());
                res.map_err(|e| pause_unknown(e, pfc_on))
            }
            QosStep::Pfc(_) if pause_changed == Some(false) => Err(StepError::Failed(
                "PFC not applied because disabling global pause failed".to_string(),
            )),
            QosStep::Pfc(_) => exec(step).map_err(|e| match (pause_changed, original_pause) {
                (Some(true), Some(orig)) => restore_pause(e, orig, &mut exec, &mut read_pause),
                _ => e,
            }),
            _ => exec(step),
        };
        match res {
            Ok(()) => {}
            Err(e) if e.is_timeout() => {
                let skipped = plan.steps.len() - i - 1;
                errors.push(format!(
                    "{e}; RoCE setup stopped, {skipped} remaining step(s) not attempted"
                ));
                break;
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    errors
}

/// Program and arguments for a step.
pub fn step_command(pf: &str, step: &QosStep) -> (&'static str, Vec<String>) {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match step {
        QosStep::DscpPrio(entries) => {
            let mut v = args(&["app", "add", "dev", pf, "dscp-prio"]);
            v.extend(entries.iter().map(|(d, p)| format!("{d}:{p}")));
            ("dcb", v)
        }
        QosStep::SetPause(p) => (
            "ethtool",
            args(&["-A", pf, "rx", on_off(p.rx), "tx", on_off(p.tx)]),
        ),
        QosStep::Pfc(prio) => {
            let mut v = args(&["pfc", "set", "dev", pf, "prio-pfc", "all:off"]);
            v.push(format!("{prio}:on"));
            ("dcb", v)
        }
        QosStep::StormTimeout(ms) => (
            "ethtool",
            args(&["--set-tunable", pf, PFC_STORM_TUNABLE, &ms.to_string()]),
        ),
    }
}

// ── I/O ──

#[derive(Debug, Default)]
pub struct QosOutcome {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// ToS write waiting for the RDMA device or rdma_cm configfs. Only the
    /// daemon retries it ([`retry::TosRetry`]); a caller without that retry
    /// must treat it as an error.
    pub pending_tos: Option<PendingTos>,
}

/// Validate `cfg`, read the PF's current QoS state, write only what differs,
/// and report problems as errors. Never fatal to the caller: VF creation
/// proceeds whatever happens here. A hung command costs at most two
/// [`CMD_TIMEOUT`]s (the failed write plus the pause restore).
///
/// `ethtool_hung`: an earlier `ethtool` command (the RX ring step) timed out,
/// so no RoCE command is issued and the ToS is deferred as pending.
pub fn apply_pf_qos(
    pf: &str,
    cfg: &RoceConfig,
    existing_vfs: Option<u32>,
    ethtool_hung: bool,
) -> QosOutcome {
    let (run, paths) = (&system_run, &TosPaths::system());
    match check_supported(pf, cfg) {
        Ok(()) => apply_checked(pf, cfg, existing_vfs, ethtool_hung, run, paths),
        Err(e) => QosOutcome {
            errors: vec![e],
            ..QosOutcome::default()
        },
    }
}

/// Runs one external command; injectable for tests.
type Runner<'a> = &'a dyn Fn(&str, &[&str]) -> Result<String, CmdError>;

/// Everything after the support check, with injected commands and paths.
fn apply_checked(
    pf: &str,
    cfg: &RoceConfig,
    existing_vfs: Option<u32>,
    ethtool_hung: bool,
    run: Runner,
    paths: &TosPaths,
) -> QosOutcome {
    let mut out = QosOutcome::default();
    if ethtool_hung {
        out.errors
            .push("roce: PF QoS not attempted because an earlier ethtool command timed out".into());
    }
    let timed_out = ethtool_hung || apply_commands(pf, cfg, existing_vfs, run, &mut out);
    match cfg.roce_tos() {
        Some(tos) => apply_tos(pf, tos, timed_out, run, paths, &mut out),
        None => out.errors.push(format!(
            "roce: RoCE default ToS not applied: invalid dscp {}",
            cfg.dscp
        )),
    }
    out
}

fn check_supported(pf: &str, cfg: &RoceConfig) -> Result<(), String> {
    cfg.validate()
        .map_err(|e| format!("roce: invalid [roce] section, PF QoS skipped: {e:#}"))?;
    match perigee_core::sysfs::read_driver_name(pf) {
        Ok(d) if d == MLX5_DRIVER => Ok(()),
        Ok(d) => Err(format!(
            "roce: PF {pf} uses driver {d}; only {MLX5_DRIVER} is supported, PF QoS left unchanged"
        )),
        Err(e) => Err(format!(
            "roce: cannot determine driver of {pf} ({e}); PF QoS left unchanged"
        )),
    }
}

/// DSCP map, pause, PFC and storm timeout. Returns whether a command timed
/// out, after which nothing further was issued.
fn apply_commands(
    pf: &str,
    cfg: &RoceConfig,
    existing_vfs: Option<u32>,
    run: Runner,
    out: &mut QosOutcome,
) -> bool {
    let (state, read_timed_out) = read_state(pf, run);
    let qos_plan = plan(cfg, &state, existing_vfs);
    out.errors
        .extend(qos_plan.errors.iter().map(|e| format!("roce: {e}")));
    if read_timed_out {
        out.errors
            .push("roce: reading the PF state timed out; no PF QoS setting was changed".into());
        return true;
    }
    for w in &qos_plan.warnings {
        warn!(pf = %pf, "{w}");
        out.warnings.push(format!("roce: {w}"));
    }
    if qos_plan.steps.is_empty() {
        info!(pf = %pf, "PF RoCE QoS: nothing to change");
        return false;
    }

    let timed_out = Cell::new(false);
    let note = |e: &StepError| timed_out.set(timed_out.get() || e.is_timeout());
    let errors = execute_plan(
        &qos_plan,
        state.pause.as_ref().ok().copied(),
        |step| {
            let res = execute_step(pf, step, run);
            match &res {
                Ok(()) => info!(pf = %pf, step = ?step, "PF RoCE QoS step applied"),
                Err(e) => warn!(pf = %pf, step = ?step, error = %e, "PF RoCE QoS step failed"),
            }
            res.inspect_err(note)
        },
        || read_pause(pf, run).inspect_err(note),
    );
    out.errors
        .extend(errors.into_iter().map(|e| format!("roce: {e}")));
    timed_out.get()
}

/// The ToS write, after the commands. Deferred as pending when its
/// prerequisites are missing, or when a command already timed out (the boot
/// budget is spent; the daemon can still write it in the background).
fn apply_tos(
    pf: &str,
    tos: u8,
    after_timeout: bool,
    run: Runner,
    paths: &TosPaths,
    out: &mut QosOutcome,
) {
    let mut skip_modprobe = false;
    let attempt = if after_timeout {
        TosAttempt::Pending("not attempted because an earlier RoCE command timed out".into())
    } else {
        let load = || tos::guarded_load(&mut skip_modprobe, || modprobe_rdma_cm(run));
        tos::try_write_tos(paths, pf, tos, load)
    };
    match attempt {
        TosAttempt::Done { changed: true } => info!(pf = %pf, tos, "RoCE default ToS written"),
        TosAttempt::Done { changed: false } => {}
        TosAttempt::Pending(why) => {
            warn!(pf = %pf, tos, reason = %why, "RoCE default ToS pending");
            out.warnings
                .push(format!("roce: RoCE ToS {tos} pending: {why}"));
            out.pending_tos = Some(PendingTos {
                pf: pf.to_string(),
                tos,
                skip_modprobe,
            });
        }
        TosAttempt::Failed(e) => out
            .errors
            .push(format!("roce: RoCE default ToS not applied: {e}")),
    }
}

/// Read the PF's QoS state. After the first timeout the remaining reads are
/// skipped (the tool is hung); returns whether that happened.
fn read_state(pf: &str, run: Runner) -> (PfQosState, bool) {
    let mut timed_out = false;
    let mut cmd = |program: &str, args: &[&str]| -> Result<String, String> {
        if timed_out {
            return Err("not read: an earlier RoCE command timed out".to_string());
        }
        run(program, args).map_err(|e| {
            timed_out |= e.is_timeout();
            e.to_string()
        })
    };
    let state = PfQosState {
        dscp_prio: cmd("dcb", &["-j", "app", "show", "dev", pf])
            .and_then(|o| parse_dcb_app_json(&o)),
        pfc: cmd("dcb", &["-j", "pfc", "show", "dev", pf]).and_then(|o| parse_dcb_pfc_json(&o)),
        pause: cmd("ethtool", &["-a", pf]).and_then(|o| parse_pause(&o)),
        storm_timeout_ms: cmd("ethtool", &["--get-tunable", pf, PFC_STORM_TUNABLE])
            .and_then(|o| parse_tunable(&o, PFC_STORM_TUNABLE)),
    };
    (state, timed_out)
}

fn read_pause(pf: &str, run: Runner) -> Result<Pause, StepError> {
    let out = run("ethtool", &["-a", pf])?;
    Ok(parse_pause(&out)?)
}

fn execute_step(pf: &str, step: &QosStep, run: Runner) -> Result<(), StepError> {
    let (program, args) = step_command(pf, step);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    run(program, &args_ref)
        .map(drop)
        .map_err(|e| match StepError::from(e) {
            StepError::TimedOut(m) => {
                StepError::TimedOut(format!("{m}; this PF setting is now in an unknown state"))
            }
            other => other,
        })
}

/// rdma_cm's configfs tree appears only once the module is loaded.
pub(crate) fn load_rdma_cm() -> Result<(), StepError> {
    modprobe_rdma_cm(&system_run)
}

fn modprobe_rdma_cm(run: Runner) -> Result<(), StepError> {
    run("modprobe", &["rdma_cm"])
        .map(drop)
        .map_err(StepError::from)
}

fn system_run(program: &str, args: &[&str]) -> Result<String, CmdError> {
    crate::cmd::run(program, args, CMD_TIMEOUT)
}

#[cfg(test)]
mod tests;
