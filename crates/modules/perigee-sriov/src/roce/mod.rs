//! Opt-in PF lossless-Ethernet setup for RoCE (mlx5 only).
//!
//! Pure parts (state parsers, diff planning, step execution policy, command
//! building) are separate from the thin I/O layer in [`apply_pf_qos`] so they
//! are testable without hardware.

mod parse;

use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::config::RoceConfig;
pub use parse::{parse_dcb_app_json, parse_dcb_pfc_json, parse_pause, parse_tos, parse_tunable};

const NUM_PRIORITIES: usize = 8;
const MLX5_DRIVER: &str = "mlx5_core";
const PFC_STORM_TUNABLE: &str = "pfc-prevention-tout";
const RDMA_CM_CONFIGFS: &str = "/sys/kernel/config/rdma_cm";
/// Each mlx5 PF is its own RDMA device with a single port.
const ROCE_PORT: u32 = 1;

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
    /// `Ok(None)`: no rdma_cm configfs group for the device yet (or rdma_cm
    /// not loaded), i.e. the kernel default is in effect.
    pub roce_tos: Result<Option<u8>, String>,
}

/// One write needed to reach the configured state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QosStep {
    /// DSCP -> priority entries missing from the app table.
    DscpPrio(Vec<(u8, u8)>),
    SetPause(Pause),
    /// Enable PFC on exactly this priority.
    Pfc(u8),
    StormTimeout(u16),
    RoceTos(u8),
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
/// Step order: DSCP map, pause, PFC, storm timeout, RoCE ToS. Pause goes off
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

    match (&state.roce_tos, cfg.roce_tos()) {
        (_, None) => p.errors.push(format!(
            "RoCE default ToS not applied: invalid dscp {}",
            cfg.dscp
        )),
        (Ok(Some(t)), Some(tos)) if *t == tos => {}
        (Ok(_), Some(tos)) => p.steps.push(QosStep::RoceTos(tos)),
        (Err(e), _) => p.errors.push(format!("RoCE default ToS not applied: {e}")),
    }

    p
}

// ── Execution policy ──

/// Set pause to `target`. `ethtool -A` exits non-zero when it considers the
/// request a no-op, so a failure is re-checked against the re-read state.
fn set_pause(
    target: Pause,
    exec: &mut impl FnMut(&QosStep) -> Result<(), String>,
    read_pause: &mut impl FnMut() -> Result<Pause, String>,
) -> Result<(), String> {
    exec(&QosStep::SetPause(target)).or_else(|e| match read_pause() {
        Ok(now) if now == target => Ok(()),
        _ => Err(e),
    })
}

/// Run the plan's steps through `exec`, returning the errors. A failing step
/// does not stop the independent ones after it, except that PFC is skipped
/// when pause could not be turned off, and a failed PFC write restores the
/// original pause setting so the port is not left without flow control.
pub fn execute_plan(
    plan: &QosPlan,
    original_pause: Option<Pause>,
    mut exec: impl FnMut(&QosStep) -> Result<(), String>,
    mut read_pause: impl FnMut() -> Result<Pause, String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    // None: no pause step in the plan; Some(ok): whether it succeeded.
    let mut pause_changed: Option<bool> = None;
    for step in &plan.steps {
        match step {
            QosStep::SetPause(target) => {
                let res = set_pause(*target, &mut exec, &mut read_pause);
                pause_changed = Some(res.is_ok());
                if let Err(e) = res {
                    errors.push(e);
                }
            }
            QosStep::Pfc(_) if pause_changed == Some(false) => {
                errors.push("PFC not applied because disabling global pause failed".to_string())
            }
            QosStep::Pfc(_) => {
                if let Err(e) = exec(step) {
                    errors.push(match (pause_changed, original_pause) {
                        (Some(true), Some(orig)) => {
                            match set_pause(orig, &mut exec, &mut read_pause) {
                                Ok(()) => format!("{e}; global pause restored to {orig}"),
                                Err(e2) => format!(
                                    "{e}; restoring global pause ({orig}) also failed: {e2}. \
                                     The port currently has NO flow control (neither PFC nor \
                                     global pause)"
                                ),
                            }
                        }
                        _ => e,
                    });
                }
            }
            _ => {
                if let Err(e) = exec(step) {
                    errors.push(e);
                }
            }
        }
    }
    errors
}

/// Program and arguments for a step; `None` for the configfs ToS write.
pub fn step_command(pf: &str, step: &QosStep) -> Option<(&'static str, Vec<String>)> {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match step {
        QosStep::DscpPrio(entries) => {
            let mut v = args(&["app", "add", "dev", pf, "dscp-prio"]);
            v.extend(entries.iter().map(|(d, p)| format!("{d}:{p}")));
            Some(("dcb", v))
        }
        QosStep::SetPause(p) => Some((
            "ethtool",
            args(&["-A", pf, "rx", on_off(p.rx), "tx", on_off(p.tx)]),
        )),
        QosStep::Pfc(prio) => {
            let mut v = args(&["pfc", "set", "dev", pf, "prio-pfc", "all:off"]);
            v.push(format!("{prio}:on"));
            Some(("dcb", v))
        }
        QosStep::StormTimeout(ms) => Some((
            "ethtool",
            args(&["--set-tunable", pf, PFC_STORM_TUNABLE, &ms.to_string()]),
        )),
        QosStep::RoceTos(_) => None,
    }
}

// ── I/O ──

#[derive(Debug, Default)]
pub struct QosOutcome {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Validate `cfg`, read the PF's current QoS state, write only what differs,
/// and report problems as errors. Never fatal to the caller: VF creation
/// proceeds whatever happens here.
pub fn apply_pf_qos(pf: &str, cfg: &RoceConfig, existing_vfs: Option<u32>) -> QosOutcome {
    let mut out = QosOutcome::default();
    if let Err(e) = cfg.validate() {
        out.errors.push(format!(
            "roce: invalid [roce] section, PF QoS skipped: {e:#}"
        ));
        return out;
    }
    match perigee_core::sysfs::read_driver_name(pf) {
        Ok(d) if d == MLX5_DRIVER => {}
        Ok(d) => {
            out.errors.push(format!(
                "roce: PF {pf} uses driver {d}; only {MLX5_DRIVER} is supported, PF QoS left unchanged"
            ));
            return out;
        }
        Err(e) => {
            out.errors.push(format!(
                "roce: cannot determine driver of {pf} ({e}); PF QoS left unchanged"
            ));
            return out;
        }
    }

    let ibdev = find_rdma_device(pf);
    let state = read_state(pf, &ibdev);
    let qos_plan = plan(cfg, &state, existing_vfs);
    out.errors
        .extend(qos_plan.errors.iter().map(|e| format!("roce: {e}")));
    for w in &qos_plan.warnings {
        warn!(pf = %pf, "{w}");
        out.warnings.push(format!("roce: {w}"));
    }
    if qos_plan.steps.is_empty() {
        info!(pf = %pf, "PF RoCE QoS: nothing to change");
        return out;
    }

    let errors = execute_plan(
        &qos_plan,
        state.pause.as_ref().ok().copied(),
        |step| {
            let res = execute_step(pf, ibdev.as_deref().ok(), step);
            match &res {
                Ok(()) => info!(pf = %pf, step = ?step, "PF RoCE QoS step applied"),
                Err(e) => warn!(pf = %pf, step = ?step, error = %e, "PF RoCE QoS step failed"),
            }
            res
        },
        || read_pause(pf),
    );
    out.errors
        .extend(errors.into_iter().map(|e| format!("roce: {e}")));
    out
}

fn read_state(pf: &str, ibdev: &Result<String, String>) -> PfQosState {
    PfQosState {
        dscp_prio: run("dcb", &["-j", "app", "show", "dev", pf])
            .and_then(|o| parse_dcb_app_json(&o)),
        pfc: run("dcb", &["-j", "pfc", "show", "dev", pf]).and_then(|o| parse_dcb_pfc_json(&o)),
        pause: read_pause(pf),
        storm_timeout_ms: run("ethtool", &["--get-tunable", pf, PFC_STORM_TUNABLE])
            .and_then(|o| parse_tunable(&o, PFC_STORM_TUNABLE)),
        roce_tos: ibdev.clone().and_then(|d| read_roce_tos(&d)),
    }
}

fn read_pause(pf: &str) -> Result<Pause, String> {
    run("ethtool", &["-a", pf]).and_then(|o| parse_pause(&o))
}

fn execute_step(pf: &str, ibdev: Option<&str>, step: &QosStep) -> Result<(), String> {
    match (step, step_command(pf, step)) {
        (QosStep::RoceTos(tos), _) => {
            write_roce_tos(ibdev.ok_or("no RDMA device for the PF")?, *tos)
        }
        (_, Some((program, args))) => {
            let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
            run(program, &args_ref).map(drop)
        }
        (_, None) => Err(format!("no command for {step:?}")),
    }
}

/// RDMA device backing the PF (e.g. mlx5_0).
fn find_rdma_device(pf: &str) -> Result<String, String> {
    let dir = format!("/sys/class/net/{pf}/device/infiniband");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("cannot list {dir}: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .next()
        .ok_or_else(|| format!("no RDMA device under {dir}"))
}

fn roce_tos_path(root: &Path, ibdev: &str) -> PathBuf {
    root.join(ibdev)
        .join("ports")
        .join(ROCE_PORT.to_string())
        .join("default_roce_tos")
}

/// Side-effect free: a missing rdma_cm tree or device group means the kernel
/// default ToS is in effect, which the write path then changes.
fn read_roce_tos(ibdev: &str) -> Result<Option<u8>, String> {
    let root = Path::new(RDMA_CM_CONFIGFS);
    if !root.join(ibdev).is_dir() {
        return Ok(None);
    }
    let path = roce_tos_path(root, ibdev);
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_tos(&content).map(Some)
}

/// rdma_cm's configfs tree appears only once the module is loaded.
fn ensure_rdma_cm_configfs() -> Result<&'static Path, String> {
    let root = Path::new(RDMA_CM_CONFIGFS);
    if root.is_dir() {
        return Ok(root);
    }
    run("modprobe", &["rdma_cm"])
        .map_err(|e| format!("{RDMA_CM_CONFIGFS} missing and loading rdma_cm failed: {e}"))?;
    if root.is_dir() {
        Ok(root)
    } else {
        Err(format!(
            "{RDMA_CM_CONFIGFS} still missing after `modprobe rdma_cm` (is configfs mounted?)"
        ))
    }
}

fn write_roce_tos(ibdev: &str, tos: u8) -> Result<(), String> {
    let root = ensure_rdma_cm_configfs()?;
    let group = root.join(ibdev);
    // mkdir in rdma_cm configfs creates the device group and its ports/.
    if !group.is_dir() {
        std::fs::create_dir(&group)
            .map_err(|e| format!("cannot create {}: {e}", group.display()))?;
    }
    let path = roce_tos_path(root, ibdev);
    std::fs::write(&path, tos.to_string())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Run a command and return its stdout, or a message naming the command.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run `{program}`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests;
