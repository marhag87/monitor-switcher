//! Running a step in a child process, so that a crash or a hang in it cannot
//! take this process down with it.
//!
//! This exists for libCEC. Its worker thread can dereference a null pointer
//! while a connection is being closed — seen when switching back to the TV a
//! few seconds after sending it to standby — and an access violation on a
//! thread we do not own is not something Rust, or even structured exception
//! handling, can catch and carry on from: libCEC would be left half torn down,
//! still holding the adapter's serial port. In a child process the same fault
//! is an exit code, and the operating system closes the port for us.
//!
//! It is also the only cure for a hang. A call into libCEC that never returns
//! cannot be interrupted inside this process; a child can be killed.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::System::Threading::CREATE_NO_WINDOW;

/// How often to look in on a running child. A wake takes seconds, so this only
/// needs to be small next to that.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How a child process ended.
#[derive(Debug)]
pub enum Finished {
    /// It exited by itself: normally, with an error, or by crashing.
    Exited {
        code: u32,
        stdout: String,
        stderr: String,
    },
    /// It was still running at the deadline, so it was killed.
    TimedOut,
}

/// Run `command` to completion, or kill it once `timeout` has passed.
///
/// Its output is captured rather than inherited, and it gets no console window:
/// the tray daemon has no console of its own, so a console program started from
/// it would otherwise flash one up on screen. An error means the child could
/// not be started at all.
pub fn run(mut command: Command, timeout: Duration) -> std::io::Result<Finished> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()?;

    // Both pipes are drained while the child runs, not after: a child that
    // fills a pipe's buffer blocks until someone reads it, and would then look
    // exactly like a hang.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // The readers are left to finish on their own: the pipes close
            // when the process does, and nothing it wrote is wanted now.
            return Ok(Finished::TimedOut);
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    Ok(Finished::Exited {
        // Windows exit codes are unsigned 32-bit values; std hands them over as
        // i32, which would turn every crash code negative.
        code: status.code().unwrap_or_default() as u32,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// What a crash looks like from outside, if this exit code is one.
///
/// A process killed by an unhandled exception exits with the exception's
/// NTSTATUS code, and those all carry the error severity in their top two bits.
/// An ordinary failure exits with something small — 1 for this tool's own
/// errors, 101 for a Rust panic — so the two cannot be confused.
pub fn crash(code: u32) -> Option<String> {
    if code & 0xC000_0000 != 0xC000_0000 {
        return None;
    }
    let what = match code {
        0xC000_0005 => "an access violation",
        0xC000_001D => "an illegal instruction",
        0xC000_00FD => "a stack overflow",
        0xC000_0374 => "heap corruption",
        0xC000_0409 => "a fail-fast abort",
        _ => "an unhandled exception",
    };
    Some(format!("{what} (0x{code:08X})"))
}

/// Fold an error report printed by this tool's CLI onto one line.
///
/// The CLI prints `error: <what>` followed by one `  caused by: <why>` line per
/// cause. A note in the tray log is a single line, so this joins them the way
/// anyhow's `{:#}` would have done in-process.
pub fn one_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .or_else(|| line.strip_prefix("caused by:"))
                .unwrap_or(line)
                .trim()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(script: &str) -> Command {
        let mut c = Command::new("cmd");
        c.args(["/d", "/c", script]);
        c
    }

    const PLENTY: Duration = Duration::from_secs(20);

    #[test]
    fn output_and_exit_code_come_back_separately() {
        match run(cmd("echo out& echo err 1>&2& exit 3"), PLENTY).unwrap() {
            Finished::Exited {
                code,
                stdout,
                stderr,
            } => {
                assert_eq!(code, 3);
                assert_eq!(stdout.trim(), "out");
                assert_eq!(stderr.trim(), "err");
            }
            other => panic!("expected an exit, got {other:?}"),
        }
    }

    /// The case this module exists for: the child dies of an access violation,
    /// and the parent sees an exit code it can name rather than dying too.
    #[test]
    fn a_crash_is_reported_as_an_exit_code() {
        match run(cmd("exit -1073741819"), PLENTY).unwrap() {
            Finished::Exited { code, .. } => {
                assert_eq!(code, 0xC000_0005);
                assert_eq!(
                    crash(code).as_deref(),
                    Some("an access violation (0xC0000005)")
                );
            }
            other => panic!("expected an exit, got {other:?}"),
        }
    }

    /// The other case: a child that never finishes is killed at the deadline,
    /// rather than holding up its caller for as long as it likes.
    #[test]
    fn a_child_still_running_at_the_deadline_is_killed() {
        let mut slow = Command::new("ping");
        slow.args(["-n", "30", "127.0.0.1"]);

        let started = Instant::now();
        let finished = run(slow, Duration::from_millis(300)).unwrap();
        assert!(matches!(finished, Finished::TimedOut), "got {finished:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the child was waited on instead of killed"
        );
    }

    #[test]
    fn a_program_that_does_not_exist_is_an_error_not_a_hang() {
        let missing = Command::new("this-program-does-not-exist-anywhere");
        assert!(run(missing, PLENTY).is_err());
    }

    #[test]
    fn only_error_severity_codes_are_crashes() {
        assert_eq!(crash(0), None);
        assert_eq!(crash(1), None);
        assert_eq!(crash(101), None);
        // A warning-severity status is not a crash.
        assert_eq!(crash(0x8000_0005), None);
        assert_eq!(
            crash(0xC000_0409).as_deref(),
            Some("a fail-fast abort (0xC0000409)")
        );
        assert_eq!(
            crash(0xC123_4567).as_deref(),
            Some("an unhandled exception (0xC1234567)")
        );
    }

    #[test]
    fn a_cli_error_report_folds_onto_one_line() {
        let report = "error: no CEC adapter found (is the Pulse-Eight adapter plugged in?)\n  \
                      caused by: could not open the connection\n";
        assert_eq!(
            one_line(report),
            "no CEC adapter found (is the Pulse-Eight adapter plugged in?): \
             could not open the connection"
        );
        assert_eq!(one_line(""), "");
        assert_eq!(one_line("plain text\n"), "plain text");
    }
}
