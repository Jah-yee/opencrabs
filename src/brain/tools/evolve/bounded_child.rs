//! Running a child process under a hard wall-clock budget.
//!
//! Extracted from the Homebrew evolve branch (#1779), which awaited `brew
//! update` and `brew upgrade` with no budget at all: a brew wedged on a locked
//! curl or a dead mirror held the evolve open and the operator received neither
//! success nor failure. The supervision is generic because the failure mode is
//! not: any external upgrade child can hang, and the download branch shells out
//! too.
//!
//! The program is a parameter rather than a hardcoded `"brew"` so the kill path
//! is testable against a child that really hangs, on any host, with no Homebrew
//! install to reproduce against.
use std::process::Output;
use std::time::Duration;

/// How one bounded child ended.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The child exited inside the budget. The exit status may still be failure.
    Exited(Output),
    /// The budget expired and the child was killed.
    TimedOut,
}

/// Run `program` with `args`, stdin detached and both output pipes captured,
/// under `budget`.
///
/// The explicit `piped()` calls are load-bearing, and this is where it bit:
/// tokio's `Command` does NOT inherit std's defaults. `std::process::Command::
/// output()` pipes stdout/stderr for you; `tokio::process::Command::spawn()`
/// leaves them INHERITED, and a `Child` whose stdout is `None` hands
/// `wait_with_output()` an empty `Vec` while the child's text goes to the
/// parent's stderr. So without the two lines below every `brew` message landed
/// in the daemon log and the evolve saw an empty report: `upgrade_failure_message`
/// would quote nothing, and the version read-back would always be `None`.
///
/// `kill_on_drop` is the other load-bearing flag. On timeout the
/// `wait_with_output` future is dropped; without it the child survives as an
/// orphan while the caller reports a timeout, and the next invocation collides on
/// the tool's own lock (brew's is a `HOMEBREW_LOCK`), turning one hang into two.
///
/// `Err` is a spawn failure only. A child that ran and exited non-zero is
/// [`Outcome::Exited`] carrying a failing status, because the caller words those
/// two outcomes differently.
pub(crate) async fn run_bounded(
    program: &str,
    args: &[&str],
    budget: Duration,
) -> std::io::Result<Outcome> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn()?;
    match tokio::time::timeout(budget, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(Outcome::Exited(output)),
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => Ok(Outcome::TimedOut),
    }
}

/// Take the version field out of a `<name> <version>` line, ignoring blanks.
///
/// `None` for anything that is not a clean two-field line, including a bare name
/// (the tool printing what it knows about with no keg installed) and the empty
/// string. Guessing here would put a version in front of the user that nobody
/// verified, which is the exact defect this exists to remove.
pub(crate) fn first_version_field(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut fields = line.split_whitespace();
    let _name = fields.next()?;
    let version = fields.next()?;
    Some(version.to_string())
}
