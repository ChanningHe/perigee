use super::*;

const PAUSE_ON: Pause = Pause { rx: true, tx: true };

fn cfg() -> RoceConfig {
    RoceConfig {
        enabled: true,
        ..RoceConfig::default()
    }
}

/// serverhub today: nothing configured, global pause on, mlx5 default storm
/// timeout.
fn serverhub_state() -> PfQosState {
    PfQosState {
        dscp_prio: Ok(vec![]),
        pfc: Ok([false; 8]),
        pause: Ok(PAUSE_ON),
        storm_timeout_ms: Ok(8000),
    }
}

fn converged_state() -> PfQosState {
    PfQosState {
        dscp_prio: Ok(vec![(26, 3), (48, 6)]),
        pfc: Ok([false, false, false, true, false, false, false, false]),
        pause: Ok(PAUSE_OFF),
        storm_timeout_ms: Ok(100),
    }
}

/// Plan as at boot: no VFs yet.
fn boot_plan(state: &PfQosState) -> QosPlan {
    plan(&cfg(), state, Some(0))
}

// ── planning: what differs ──

#[test]
fn fresh_host_at_boot_plans_every_step_in_order() {
    let p = boot_plan(&serverhub_state());
    assert_eq!(
        p.steps,
        vec![
            QosStep::DscpPrio(vec![(26, 3), (48, 6)]),
            QosStep::SetPause(PAUSE_OFF),
            QosStep::Pfc(3),
            QosStep::StormTimeout(100),
        ]
    );
    assert!(p.errors.is_empty());
    assert!(p.warnings.is_empty(), "no VFs: nothing to warn about");
}

#[test]
fn converged_host_plans_nothing() {
    assert_eq!(boot_plan(&converged_state()), QosPlan::default());
    assert_eq!(
        plan(&cfg(), &converged_state(), Some(64)),
        QosPlan::default()
    );
}

#[test]
fn matching_pfc_is_never_reissued() {
    let mut s = converged_state();
    s.storm_timeout_ms = Ok(8000);
    assert_eq!(boot_plan(&s).steps, vec![QosStep::StormTimeout(100)]);
}

#[test]
fn extra_pfc_priority_counts_as_difference() {
    let mut s = converged_state();
    s.pfc = Ok([false, false, false, true, true, false, false, false]);
    assert_eq!(boot_plan(&s).steps, vec![QosStep::Pfc(3)]);
}

#[test]
fn only_missing_app_entries_are_added_and_others_kept() {
    let mut s = converged_state();
    s.dscp_prio = Ok(vec![(10, 1), (26, 3)]);
    assert_eq!(boot_plan(&s).steps, vec![QosStep::DscpPrio(vec![(48, 6)])]);
}

#[test]
fn remapped_dscp_is_re_added() {
    let mut s = converged_state();
    s.dscp_prio = Ok(vec![(26, 5), (48, 6)]);
    assert_eq!(boot_plan(&s).steps, vec![QosStep::DscpPrio(vec![(26, 3)])]);
}

#[test]
fn half_on_pause_is_turned_off_when_pfc_already_on() {
    let mut s = converged_state();
    s.pause = Ok(Pause {
        rx: false,
        tx: true,
    });
    assert_eq!(boot_plan(&s).steps, vec![QosStep::SetPause(PAUSE_OFF)]);
    // PFC already on: turning pause off does not flap, so it is done live too.
    assert_eq!(
        plan(&cfg(), &s, Some(64)).steps,
        vec![QosStep::SetPause(PAUSE_OFF)]
    );
}

#[test]
fn dscp_map_follows_configured_dscp() {
    let mut c = cfg();
    c.dscp = 24;
    let p = plan(&c, &converged_state(), Some(0));
    assert_eq!(p.steps, vec![QosStep::DscpPrio(vec![(24, 3)])]);
}

#[test]
fn unreadable_state_skips_step_with_error() {
    let mut s = serverhub_state();
    s.dscp_prio = Err("dcb: not found".into());
    s.storm_timeout_ms = Err("unsupported".into());
    let p = boot_plan(&s);
    assert_eq!(p.steps, vec![QosStep::SetPause(PAUSE_OFF), QosStep::Pfc(3)]);
    assert_eq!(p.errors.len(), 2);
    assert!(p.errors.iter().any(|e| e.contains("dcb: not found")));
}

#[test]
fn unreadable_pfc_leaves_pfc_and_pause_alone() {
    // Turning pause off without knowing PFC is on could leave no flow control.
    let mut s = serverhub_state();
    s.pfc = Err("dcb failed".into());
    let p = boot_plan(&s);
    assert!(!p
        .steps
        .iter()
        .any(|st| matches!(st, QosStep::Pfc(_) | QosStep::SetPause(_))));
    assert_eq!(p.errors.len(), 1);
}

#[test]
fn unknown_pause_state_blocks_pfc() {
    let mut s = serverhub_state();
    s.pause = Err("ethtool: not found".into());
    let p = boot_plan(&s);
    assert!(!p.steps.contains(&QosStep::Pfc(3)));
    assert!(!p.steps.iter().any(|st| matches!(st, QosStep::SetPause(_))));
    assert_eq!(p.errors.len(), 2);
}

// ── planning: live host (VFs exist) ──

#[test]
fn live_host_defers_pfc_and_pause_but_applies_the_rest() {
    let p = plan(&cfg(), &serverhub_state(), Some(64));
    assert_eq!(
        p.steps,
        vec![
            QosStep::DscpPrio(vec![(26, 3), (48, 6)]),
            QosStep::StormTimeout(100),
        ]
    );
    assert_eq!(p.errors.len(), 1);
    assert!(p.errors[0].contains("64 VFs exist"));
    assert!(p.errors[0].contains("next boot"));
}

#[test]
fn unknown_vf_count_is_treated_as_live() {
    let p = plan(&cfg(), &serverhub_state(), None);
    assert!(!p
        .steps
        .iter()
        .any(|st| matches!(st, QosStep::Pfc(_) | QosStep::SetPause(_))));
    assert!(p.errors[0].contains("could not be read"));
}

#[test]
fn first_dscp_entry_on_live_host_warns_about_trust_switch() {
    let live = plan(&cfg(), &serverhub_state(), Some(64));
    assert_eq!(live.warnings.len(), 1);
    assert!(live.warnings[0].contains("DSCP trust"));
    // Table already has entries: trust is already DSCP, no warning.
    let mut s = serverhub_state();
    s.dscp_prio = Ok(vec![(26, 3)]);
    assert!(plan(&cfg(), &s, Some(64)).warnings.is_empty());
}

// ── command building ──

fn cmd(step: QosStep) -> (&'static str, Vec<String>) {
    step_command("enp65s0np0", &step)
}

fn strings(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn builds_dcb_commands() {
    assert_eq!(
        cmd(QosStep::DscpPrio(vec![(26, 3), (48, 6)])),
        (
            "dcb",
            strings(&[
                "app",
                "add",
                "dev",
                "enp65s0np0",
                "dscp-prio",
                "26:3",
                "48:6"
            ])
        )
    );
    assert_eq!(
        cmd(QosStep::Pfc(3)),
        (
            "dcb",
            strings(&[
                "pfc",
                "set",
                "dev",
                "enp65s0np0",
                "prio-pfc",
                "all:off",
                "3:on"
            ])
        )
    );
}

#[test]
fn builds_ethtool_commands() {
    assert_eq!(
        cmd(QosStep::SetPause(PAUSE_OFF)),
        (
            "ethtool",
            strings(&["-A", "enp65s0np0", "rx", "off", "tx", "off"])
        )
    );
    assert_eq!(
        cmd(QosStep::SetPause(PAUSE_ON)),
        (
            "ethtool",
            strings(&["-A", "enp65s0np0", "rx", "on", "tx", "on"])
        )
    );
    assert_eq!(
        cmd(QosStep::StormTimeout(100)),
        (
            "ethtool",
            strings(&["--set-tunable", "enp65s0np0", "pfc-prevention-tout", "100"])
        )
    );
}

// ── execution policy (injected executor) ──

/// Run the boot plan for serverhub with `fail` deciding which exec calls fail
/// and `pause_now` what a pause re-read returns. Returns executed steps and
/// errors.
fn run_boot(fail: impl Fn(&QosStep) -> bool, pause_now: Pause) -> (Vec<QosStep>, Vec<String>) {
    let p = boot_plan(&serverhub_state());
    let mut calls = Vec::new();
    let errors = execute_plan(
        &p,
        Some(PAUSE_ON),
        |s| {
            calls.push(s.clone());
            if fail(s) {
                Err(StepError::Failed(format!("{s:?} failed")))
            } else {
                Ok(())
            }
        },
        || Ok(pause_now),
    );
    (calls, errors)
}

#[test]
fn all_steps_run_in_order_when_nothing_fails() {
    let (calls, errors) = run_boot(|_| false, PAUSE_OFF);
    assert_eq!(calls, boot_plan(&serverhub_state()).steps);
    assert!(errors.is_empty());
}

#[test]
fn pause_off_failure_confirmed_by_reread_skips_pfc() {
    let (calls, errors) = run_boot(|s| *s == QosStep::SetPause(PAUSE_OFF), PAUSE_ON);
    assert!(!calls.contains(&QosStep::Pfc(3)));
    assert!(
        calls.contains(&QosStep::StormTimeout(100)),
        "independent steps still run"
    );
    assert_eq!(errors.len(), 2);
    assert!(errors[1].contains("PFC not applied"));
}

#[test]
fn pause_off_nonzero_exit_but_reread_off_continues() {
    let (calls, errors) = run_boot(|s| *s == QosStep::SetPause(PAUSE_OFF), PAUSE_OFF);
    assert!(calls.contains(&QosStep::Pfc(3)));
    assert!(errors.is_empty());
}

#[test]
fn pfc_failure_restores_original_pause() {
    let (calls, errors) = run_boot(|s| *s == QosStep::Pfc(3), PAUSE_OFF);
    let pfc_at = calls.iter().position(|s| *s == QosStep::Pfc(3)).unwrap();
    assert_eq!(calls[pfc_at + 1], QosStep::SetPause(PAUSE_ON));
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("restored to rx on tx on"));
}

#[test]
fn failed_pause_restore_says_port_has_no_flow_control() {
    let (_, errors) = run_boot(
        |s| matches!(s, QosStep::Pfc(_)) || *s == QosStep::SetPause(PAUSE_ON),
        PAUSE_OFF,
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("NO flow control"));
}

#[test]
fn pfc_failure_without_pause_change_does_not_touch_pause() {
    let mut s = serverhub_state();
    s.pause = Ok(PAUSE_OFF);
    let p = boot_plan(&s);
    let mut calls = Vec::new();
    let errors = execute_plan(
        &p,
        Some(PAUSE_OFF),
        |st| {
            calls.push(st.clone());
            if matches!(st, QosStep::Pfc(_)) {
                Err(StepError::Failed("pfc failed".into()))
            } else {
                Ok(())
            }
        },
        || Ok(PAUSE_OFF),
    );
    assert!(!calls.iter().any(|st| matches!(st, QosStep::SetPause(_))));
    assert_eq!(errors, vec!["pfc failed".to_string()]);
}

#[test]
fn live_plan_never_executes_pfc_or_pause() {
    let p = plan(&cfg(), &serverhub_state(), Some(64));
    let mut calls = Vec::new();
    execute_plan(
        &p,
        Some(PAUSE_ON),
        |st| {
            calls.push(st.clone());
            Ok(())
        },
        || Ok(PAUSE_ON),
    );
    assert!(!calls
        .iter()
        .any(|st| matches!(st, QosStep::Pfc(_) | QosStep::SetPause(_))));
}

// ── execution policy: timeouts ──

fn timed_out(s: &QosStep) -> StepError {
    StepError::TimedOut(format!("{s:?} timed out"))
}

/// Boot plan with `outcome` per exec call and `pause_now` per re-read.
/// Returns executed steps, number of pause re-reads, and errors.
fn run_boot_with(
    outcome: impl Fn(&QosStep) -> Result<(), StepError>,
    pause_now: Result<Pause, StepError>,
) -> (Vec<QosStep>, usize, Vec<String>) {
    let p = boot_plan(&serverhub_state());
    let mut calls = Vec::new();
    let mut rereads = 0;
    let errors = execute_plan(
        &p,
        Some(PAUSE_ON),
        |s| {
            calls.push(s.clone());
            outcome(s)
        },
        || {
            rereads += 1;
            pause_now.clone()
        },
    );
    (calls, rereads, errors)
}

#[test]
fn timeout_stops_the_plan_at_that_step() {
    let (calls, _, errors) = run_boot_with(
        |s| match s {
            QosStep::DscpPrio(_) => Err(timed_out(s)),
            _ => Ok(()),
        },
        Ok(PAUSE_OFF),
    );
    assert_eq!(calls.len(), 1, "nothing after the hung command: {calls:?}");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("3 remaining step(s) not attempted"));
}

#[test]
fn pause_timeout_is_not_re_read_and_skips_pfc() {
    let (calls, rereads, errors) = run_boot_with(
        |s| match s {
            QosStep::SetPause(_) => Err(timed_out(s)),
            _ => Ok(()),
        },
        Ok(PAUSE_OFF),
    );
    assert_eq!(rereads, 0);
    assert_eq!(calls.last(), Some(&QosStep::SetPause(PAUSE_OFF)));
    assert!(!calls.contains(&QosStep::Pfc(3)));
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("may have NO flow control"),
        "{}",
        errors[0]
    );
}

#[test]
fn pause_reread_timeout_also_stops_the_plan() {
    let (calls, rereads, errors) = run_boot_with(
        |s| match s {
            QosStep::SetPause(_) => Err(StepError::Failed("ethtool: exit 1".into())),
            _ => Ok(()),
        },
        Err(StepError::TimedOut("ethtool -a timed out".into())),
    );
    assert_eq!(rereads, 1);
    assert!(!calls.contains(&QosStep::Pfc(3)));
    assert!(errors[0].contains("re-reading global pause"));
    assert!(errors[0].contains("not attempted"));
}

#[test]
fn pfc_timeout_still_restores_pause_once_then_stops() {
    let (calls, rereads, errors) = run_boot_with(
        |s| match s {
            QosStep::Pfc(_) => Err(timed_out(s)),
            _ => Ok(()),
        },
        Ok(PAUSE_OFF),
    );
    assert_eq!(
        &calls[1..],
        &[
            QosStep::SetPause(PAUSE_OFF),
            QosStep::Pfc(3),
            QosStep::SetPause(PAUSE_ON),
        ],
        "storm timeout is not attempted after the hang"
    );
    assert_eq!(rereads, 0);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("restored to rx on tx on"));
    assert!(errors[0].contains("1 remaining step(s) not attempted"));
}

#[test]
fn pfc_timeout_with_failed_restore_says_flow_control_may_be_gone() {
    let (calls, _, errors) = run_boot_with(
        |s| match s {
            QosStep::Pfc(_) => Err(timed_out(s)),
            QosStep::SetPause(p) if *p == PAUSE_ON => Err(timed_out(s)),
            _ => Ok(()),
        },
        Ok(PAUSE_OFF),
    );
    assert_eq!(
        calls
            .iter()
            .filter(|s| **s == QosStep::SetPause(PAUSE_ON))
            .count(),
        1,
        "restore attempted exactly once"
    );
    assert!(errors[0].contains("may have NO flow control"));
}

// ── I/O glue with an injected command runner ──

use super::tos::test_env::{FakeSys, PF};
use crate::cmd::CmdError;
use std::cell::RefCell;

const PFC_OFF_JSON: &str = r#"{"prio_pfc":[false,false,false,false,false,false,false,false]}"#;
const PAUSE_ON_TEXT: &str = "Pause parameters for x:\nAutonegotiate:\toff\nRX:\t\ton\nTX:\t\ton\n";

/// serverhub's PF as seen by the tools; commands starting with `hang` time
/// out. Every command line is recorded in `calls`.
fn fake_run<'a>(
    calls: &'a RefCell<Vec<String>>,
    hang: &'a str,
) -> impl Fn(&str, &[&str]) -> Result<String, CmdError> + 'a {
    move |program, args| {
        let cmd = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        calls.borrow_mut().push(cmd.clone());
        if !hang.is_empty() && cmd.starts_with(hang) {
            return Err(CmdError::TimedOut {
                cmd,
                timeout: CMD_TIMEOUT,
            });
        }
        let out = match cmd.split(' ').take(3).collect::<Vec<_>>()[..] {
            ["dcb", "-j", "app"] => "{}",
            ["dcb", "-j", "pfc"] => PFC_OFF_JSON,
            ["ethtool", "-a", _] => PAUSE_ON_TEXT,
            ["ethtool", "--get-tunable", _] => "pfc-prevention-tout: 8000\n",
            _ => "",
        };
        Ok(out.to_string())
    }
}

fn apply_with(sys: &FakeSys, hang: &str) -> (QosOutcome, Vec<String>) {
    apply_after(sys, hang, false)
}

fn apply_after(sys: &FakeSys, hang: &str, ethtool_hung: bool) -> (QosOutcome, Vec<String>) {
    let calls = RefCell::new(Vec::new());
    let out = {
        let run = fake_run(&calls, hang);
        apply_checked(PF, &cfg(), Some(0), ethtool_hung, &run, &sys.paths)
    };
    (out, calls.into_inner())
}

#[test]
fn ring_step_timeout_skips_all_roce_commands_and_defers_tos() {
    let sys = FakeSys::new();
    sys.add_rdma_device();
    sys.add_configfs_group("0");
    let (out, calls) = apply_after(&sys, "", true);
    assert!(calls.is_empty(), "{calls:?}");
    assert_eq!(sys.read_tos(), "0");
    assert!(out.pending_tos.is_some());
    assert!(out.errors.iter().any(|e| e.contains("not attempted")));
}

fn ran(calls: &[String], prefix: &str) -> bool {
    calls.iter().any(|c| c.starts_with(prefix))
}

#[test]
fn healthy_run_issues_every_write_and_the_tos() {
    let sys = FakeSys::new();
    sys.add_rdma_device();
    sys.add_configfs_group("0");
    let (out, calls) = apply_with(&sys, "");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(calls.len(), 8, "4 reads + 4 writes: {calls:?}");
    assert_eq!(sys.read_tos(), "106");
    assert_eq!(out.pending_tos, None);
}

#[test]
fn command_timeout_skips_the_tos_write() {
    let sys = FakeSys::new();
    sys.add_rdma_device();
    sys.add_configfs_group("0");
    let (out, calls) = apply_with(&sys, "dcb app add");
    assert_eq!(calls.len(), 5, "4 reads + the hung write: {calls:?}");
    assert_eq!(sys.read_tos(), "0", "no ToS write after a timeout");
    assert!(out.pending_tos.is_some(), "deferred to the daemon");
    assert!(out.errors.iter().any(|e| e.contains("not attempted")));
}

#[test]
fn command_timeout_skips_modprobe() {
    let sys = FakeSys::new();
    sys.add_rdma_device(); // configfs missing: the ToS step would modprobe
    let (_, calls) = apply_with(&sys, "dcb app add");
    assert!(!ran(&calls, "modprobe"), "{calls:?}");
    let (_, calls) = apply_with(&sys, "");
    assert!(
        ran(&calls, "modprobe"),
        "control: modprobe without a timeout"
    );
}

#[test]
fn read_timeout_skips_the_remaining_reads_and_all_writes() {
    let sys = FakeSys::new();
    sys.add_rdma_device();
    sys.add_configfs_group("0");
    let (out, calls) = apply_with(&sys, "dcb -j app");
    assert_eq!(calls, ["dcb -j app show dev enp65s0np0"]);
    assert_eq!(sys.read_tos(), "0");
    assert!(out
        .errors
        .iter()
        .any(|e| e.contains("reading the PF state timed out")));
}
