//! Run external commands with a deadline.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use tracing::warn;

/// Limit for each external command in the SR-IOV apply path. The module runs
/// before READY at boot, so a hung tool must cost a bounded amount of time.
pub const CMD_TIMEOUT: Duration = Duration::from_secs(10);
/// First and longest wait between exit checks of a running child: short
/// commands are seen within about a millisecond, long ones cost few wakeups.
const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(5);
/// Time allowed after the child exited for its output to be collected.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum CmdError {
    #[error("cannot run `{cmd}`: {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: io::Error,
    },
    #[error("error while running `{cmd}`: {source}")]
    Io {
        cmd: String,
        #[source]
        source: io::Error,
    },
    #[error("`{cmd}` failed ({status}): {stderr}")]
    Failed {
        cmd: String,
        status: ExitStatus,
        stderr: String,
    },
    #[error("`{cmd}` did not finish within {timeout:?} and was killed")]
    TimedOut { cmd: String, timeout: Duration },
    #[error(
        "`{cmd}` exited ({status}) but its output pipes stayed open past {timeout:?} \
         (held by a process it left running)"
    )]
    PipesOpen {
        cmd: String,
        status: ExitStatus,
        timeout: Duration,
    },
}

impl CmdError {
    /// The deadline was overrun, whether or not the command itself exited.
    pub fn is_timeout(&self) -> bool {
        matches!(self, CmdError::TimedOut { .. } | CmdError::PipesOpen { .. })
    }
}

/// Run `program` and return its stdout, giving up after `timeout`.
///
/// Returns within about `timeout + DRAIN_GRACE` whatever the child does: on
/// timeout it is killed and abandoned (see [`abandon`]); output is collected
/// by detached reader threads that are never joined.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, CmdError> {
    let cmd = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| CmdError::Spawn {
            cmd: cmd.clone(),
            source,
        })?;
    let deadline = Instant::now() + timeout;
    // Both pipes are drained while waiting, so a chatty child never blocks
    // on a full pipe buffer.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let timed_out = || CmdError::TimedOut {
        cmd: cmd.clone(),
        timeout,
    };

    let status = match wait_until(&mut child, deadline) {
        Ok(Some(status)) => status,
        Ok(None) => {
            abandon(child);
            return Err(timed_out());
        }
        Err(source) => {
            abandon(child);
            return Err(CmdError::Io { cmd, source });
        }
    };
    // A grandchild that inherited the pipes can keep them open past the
    // child's exit; that also overruns the deadline.
    let until = deadline.max(Instant::now()) + DRAIN_GRACE;
    let to_err = |e: CollectError| e.into_cmd_error(&cmd, status, timeout);
    let out = collect(&stdout, until).map_err(to_err)?;
    let err = collect(&stderr, until).map_err(to_err)?;
    if !status.success() {
        return Err(CmdError::Failed {
            cmd,
            status,
            stderr: String::from_utf8_lossy(&err).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

type PipeResult = io::Result<Vec<u8>>;

/// Read `pipe` to EOF on a detached thread. If the thread cannot be started
/// the sender is dropped and [`collect`] reports it.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<PipeResult> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        let _ = thread::Builder::new()
            .name("cmd-output".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let res = pipe.read_to_end(&mut buf).map(|_| buf);
                // The receiver is gone if the caller already gave up.
                let _ = tx.send(res);
            });
    }
    rx
}

enum CollectError {
    TimedOut,
    Io(io::Error),
}

impl CollectError {
    fn into_cmd_error(self, cmd: &str, status: ExitStatus, timeout: Duration) -> CmdError {
        let cmd = cmd.to_string();
        match self {
            CollectError::TimedOut => CmdError::PipesOpen {
                cmd,
                status,
                timeout,
            },
            CollectError::Io(source) => CmdError::Io { cmd, source },
        }
    }
}

fn collect(rx: &Receiver<PipeResult>, until: Instant) -> Result<Vec<u8>, CollectError> {
    match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        Ok(res) => res.map_err(CollectError::Io),
        Err(RecvTimeoutError::Timeout) => Err(CollectError::TimedOut),
        Err(RecvTimeoutError::Disconnected) => Err(CollectError::Io(io::Error::other(
            "cannot start output reader thread",
        ))),
    }
}

/// Poll for exit until `deadline` with a short backoff; `Ok(None)` when it
/// is still running.
fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    let mut pause = POLL_MIN;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(POLL_MAX);
    }
}

/// SIGKILL the child and reap it on a detached thread. A process stuck in
/// uninterruptible sleep (D state, e.g. inside a hung driver call) ignores
/// SIGKILL until the kernel call returns, so `wait()` may never return: that
/// one reaper thread and process are deliberately leaked rather than blocking
/// the caller. If the thread cannot be started, the child stays a zombie
/// until the daemon exits.
fn abandon(mut child: Child) {
    if let Err(e) = child.kill() {
        warn!(pid = child.id(), error = %e, "cannot kill timed-out command");
    }
    let _ = thread::Builder::new()
        .name("cmd-reaper".into())
        .spawn(move || {
            // Only reaping; the exit status of a killed command is moot.
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const SHORT: Duration = Duration::from_millis(100);
    const GENEROUS: Duration = Duration::from_secs(5);

    #[test]
    fn returns_stdout_on_success() {
        assert_eq!(run("sh", &["-c", "echo hi"], GENEROUS).unwrap(), "hi\n");
    }

    #[test]
    fn hung_command_times_out_promptly() {
        let start = Instant::now();
        let err = run("sleep", &["5"], SHORT).unwrap_err();
        assert!(err.is_timeout(), "{err}");
        assert!(start.elapsed() < Duration::from_secs(1));
        let msg = err.to_string();
        assert!(msg.contains("`sleep 5`") && msg.contains("100ms"), "{msg}");
    }

    #[test]
    fn pipe_held_open_by_grandchild_does_not_block() {
        // The shell exits at once but its background child keeps stdout open,
        // so the readers never see EOF before the deadline.
        let start = Instant::now();
        let err = run("sh", &["-c", "sleep 5 & echo started"], SHORT).unwrap_err();
        assert!(err.is_timeout(), "{err}");
        assert!(start.elapsed() < Duration::from_secs(1));
        let msg = err.to_string();
        assert!(msg.contains("exited (exit status: 0)"), "{msg}");
        assert!(msg.contains("pipes stayed open"), "{msg}");
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        let out = run(
            "sh",
            &[
                "-c",
                "head -c 2000000 /dev/zero; head -c 200000 /dev/zero >&2",
            ],
            GENEROUS,
        )
        .unwrap();
        assert_eq!(out.len(), 2_000_000);
    }

    #[test]
    fn nonzero_exit_reports_stderr() {
        let err = run("sh", &["-c", "echo boom >&2; exit 3"], GENEROUS).unwrap_err();
        assert!(matches!(err, CmdError::Failed { ref stderr, .. } if stderr == "boom"));
        assert!(!err.is_timeout());
    }

    #[test]
    fn missing_program_is_a_spawn_error() {
        let err = run("perigee-no-such-program", &[], GENEROUS).unwrap_err();
        assert!(matches!(err, CmdError::Spawn { .. }), "{err}");
    }
}
