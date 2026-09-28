//! systemd units for the evolve restart path.
//!
//! Split out of `evolve.rs` (#963 review): these are self-contained command
//! builders with no dependency on the tool or its strategies, and pinning
//! their arg lists in tests is the whole point — silent drift in any flag
//! re-introduces the "Evolved! but the daemon never restarted" symptom.
//!
//! Also holds [`schedule_restart`], the pre-flight-and-arm sequence, since
//! that is the only consumer of the builders and it belongs with them rather
//! than inside one strategy file. It was added for the Homebrew branch (#1779),
//! which previously armed no backstop at all and relied on the in-process
//! `exec()` alone.

/// Service-unit glob used by the systemd restart path. Matches every
/// profile (default, ops, staging, ...) sharing the same binary.
pub(crate) const SYSTEMD_UNIT_PATTERN: &str = "opencrabs*.service";

/// Build the `systemd-run` command that schedules a delayed restart
/// of every service unit matching `SYSTEMD_UNIT_PATTERN`. Extracted
/// so the arg list can be pinned by tests — silent drift in any of
/// these flags would re-introduce the "Evolved! but daemon didn't
/// restart" symptom that issue #136 reported.
///
/// Set `user` to `true` to target user-level units (`systemctl --user`),
/// e.g. when OpenCrabs was installed via `install_systemd_service()` which
/// writes to `~/.config/systemd/user/`.
///
/// The `pid` argument is used to derive a unique transient unit
/// name (`opencrabs-evolve-<pid>`) so concurrent evolve calls don't
/// collide on the transient unit registry.
pub(crate) fn build_systemd_restart_command(pid: u32, user: bool) -> std::process::Command {
    let unit_name = format!("opencrabs-evolve-{pid}");
    let mut cmd = std::process::Command::new("systemd-run");
    let mut args = vec![];
    // --user on systemd-run itself is required when the daemon runs as a
    // user service: without it, systemd-run tries to talk to the system
    // bus and either fails (no permission from within a --user service)
    // or creates the transient timer in the system instance, where the
    // spawned systemctl won't have DBUS_SESSION_BUS_ADDRESS available.
    if user {
        args.push("--user".to_string());
    }
    args.push("--on-active=3".to_string());
    args.push(format!("--unit={unit_name}"));
    args.push("systemctl".to_string());
    // --user on systemctl is needed to target the user service manager.
    if user {
        args.push("--user".to_string());
    }
    args.push("restart".to_string());
    args.push(SYSTEMD_UNIT_PATTERN.to_string());
    cmd.args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Glob matching every transient evolve restart unit we have ever
/// scheduled. `build_systemd_restart_command` embeds the scheduling
/// process PID in each unit name (`opencrabs-evolve-<pid>`), so over a
/// host's lifetime many such units are created — one per evolve / auto
/// update attempt.
pub(crate) const EVOLVE_UNIT_GLOB: &str = "opencrabs-evolve-*.service";

/// Build the command that garbage-collects spent evolve restart units.
///
/// We cannot pass `--collect` to `systemd-run` (it's unsupported on
/// systemd < v240, RHEL 7 / CentOS 7), so a finished transient
/// `opencrabs-evolve-<pid>` unit lingers in systemd's registry — and a
/// restart that *fails* (e.g. it lost the channel-token race against a
/// running TUI) lingers in the **failed** state, accumulating forever.
/// `systemctl reset-failed <glob>` clears those spent units and is
/// available on every systemd version we target, so we call it right
/// before scheduling a fresh restart. Best-effort: its failure must
/// never block the evolve.
pub(crate) fn build_systemd_cleanup_command(user: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.arg("reset-failed")
        .arg(EVOLVE_UNIT_GLOB)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Count systemd service units matching the given glob pattern, at either
/// system or user level.
///
/// Set `user` to `true` to query user-level units (`systemctl --user`).
///
/// Returns `Some(n)` on a successful query (n may be zero), or
/// `None` if `systemctl` failed to spawn / returned a non-zero exit
/// status (a permissions issue or non-systemd host). `None` is a
/// "don't know" signal: the caller should fall through and schedule
/// the restart anyway rather than blocking on a diagnostic failure.
///
/// Uses `--no-legend --no-pager` to keep stdout machine-parseable.
/// Counts non-empty lines — `systemctl` prints one line per matched
/// unit when `--no-legend` is set.
pub(crate) fn count_matching_systemd_units(pattern: &str, user: bool) -> Option<usize> {
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(["list-units", "--no-legend", "--no-pager"]);
    if user {
        cmd.arg("--user");
    }
    cmd.arg(pattern);
    cmd.stderr(std::process::Stdio::null());
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(stdout.lines().filter(|l| !l.trim().is_empty()).count())
}
/// Which unit bus an evolve restart should target, or `None` when nothing
/// matched at either level.
///
/// `Some(false)` = the system bus, `Some(true)` = the user bus. The user-bus
/// fallback is the substance of #162: OpenCrabs installed as a user service
/// (`install_systemd_service()` writes to `~/.config/systemd/user/`) has zero
/// system-level units, and checking only the system bus is what produced the
/// "Evolved! but the daemon never restarted" line of reports (#136).
///
/// A `None` unit count from [`count_matching_systemd_units`] means "could not
/// tell", systemctl failed to spawn, not "zero units". That case returns
/// `Some(false)` and logs: a diagnostic failure must not withhold a restart from
/// a user whose daemon does exist. Only a confirmed zero on both buses returns
/// `None`, so the caller skips scheduling instead of quietly no-op'ing.
///
/// Both evolve branches (binary download, Homebrew) pre-flight through here so
/// the bus rules cannot drift apart.
pub(crate) fn select_unit_bus(sid: uuid::Uuid) -> Option<bool> {
    match count_matching_systemd_units(SYSTEMD_UNIT_PATTERN, false) {
        Some(0) => {}
        Some(n) => {
            tracing::info!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                matched_units = n,
                use_user_units = false,
                session_id = %sid,
                "evolve: pre-flight found {n} system-level units, scheduling restart (+3s)"
            );
            return Some(false);
        }
        None => {
            tracing::warn!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                session_id = %sid,
                "evolve: could not count system-level units (systemctl spawn failed), \
                 scheduling restart anyway"
            );
            return Some(false);
        }
    }

    match count_matching_systemd_units(SYSTEMD_UNIT_PATTERN, true) {
        Some(n) if n > 0 => {
            tracing::info!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                user_units = n,
                session_id = %sid,
                "evolve: no system-level units found, using {n} user-level units, \
                 scheduling restart with --user"
            );
            return Some(true);
        }
        _ => {}
    }

    tracing::warn!(
        target: "evolve",
        pattern = SYSTEMD_UNIT_PATTERN,
        session_id = %sid,
        "evolve: no systemd units matched the pattern (checked system and user level), \
         skipping scheduled restart"
    );
    None
}

/// Sweep spent transient evolve units before scheduling a fresh one.
///
/// Best-effort and deliberately silent about failure: a `reset-failed` that
/// cannot run must never block the restart that matters. Without it, failed
/// transient units accumulate without bound across evolves.
pub(crate) fn sweep_stale_evolve_units(use_user_units: bool, sid: uuid::Uuid) {
    match build_systemd_cleanup_command(use_user_units).status() {
        Ok(status) => tracing::debug!(
            target: "evolve",
            glob = EVOLVE_UNIT_GLOB,
            success = status.success(),
            session_id = %sid,
            "evolve: reset-failed swept stale evolve units"
        ),
        Err(e) => tracing::warn!(
            target: "evolve",
            glob = EVOLVE_UNIT_GLOB,
            error = %e,
            session_id = %sid,
            "evolve: could not sweep stale evolve units, the restart still proceeds"
        ),
    }
}
