//! Surfacing DB corruption and the recovery asset outside the TUI (#1779
//! defect 4).
//!
//! `DB_INTEGRITY_FAILED` had exactly one production consumer, the TUI banner, so
//! a headless daemon stored its verdict about a corrupted image and told nobody.
//! `doctor` was worse: it reported the image healthy without ever saying whether
//! a copy of it existed. These tests pin the reporting helpers and the two call
//! sites that wire them up.

use crate::db::migration_snapshot::{LATEST, PREFIX, newest_snapshot, newest_snapshot_note};
use std::path::Path;

/// A snapshot dir holding `names` as empty files, plus the `-latest` alias.
///
/// The files are empty on purpose: reporting never reads a snapshot's contents,
/// only its name, and an empty file keeps the test from depending on SQLite.
fn snapshot_dir_with(names: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for name in names {
        std::fs::write(tmp.path().join(name), b"").unwrap();
    }
    std::fs::write(tmp.path().join(LATEST), b"").unwrap();
    tmp
}

#[test]
fn newest_snapshot_skips_the_alias_and_takes_the_latest_dated_copy() {
    let dir = snapshot_dir_with(&[
        &format!("{PREFIX}32-20260926-010101"),
        &format!("{PREFIX}32-20260927-020202"),
        &format!("{PREFIX}33-20260928-030303"),
    ]);

    let newest = newest_snapshot(dir.path()).expect("a dated copy must be found");
    assert_eq!(
        newest.file_name().and_then(|n| n.to_str()),
        Some(format!("{PREFIX}33-20260928-030303").as_str()),
        "the newest dated copy wins: {newest:?}"
    );
    assert_ne!(
        newest.file_name().and_then(|n| n.to_str()),
        Some(LATEST),
        "the alias is a pointer, not a retention unit: naming it would report a \
         file that outlives whatever it points at"
    );
}

#[test]
fn a_missing_or_empty_snapshot_dir_reports_nothing_rather_than_failing() {
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(newest_snapshot(empty.path()), None, "no copies yet");

    let missing = empty.path().join("does-not-exist");
    assert_eq!(
        newest_snapshot(&missing),
        None,
        "an absent backups dir is the normal first-run state, not an error"
    );
}

#[test]
fn the_note_names_the_file_to_restore_or_says_there_is_none() {
    let dir = snapshot_dir_with(&[&format!("{PREFIX}33-20260928-030303")]);
    let newest = newest_snapshot(dir.path()).unwrap();

    let note = newest_snapshot_note(Some(newest.as_path()));
    assert!(
        note.contains("pre-migration-33-20260928-030303"),
        "the operator must be able to copy the file out of the message: {note}"
    );
    assert!(
        note.contains("copy it over the database file"),
        "a path without the restore verb is a puzzle, not an instruction: {note}"
    );
    assert!(
        !note.contains(LATEST),
        "the alias must never be the thing an operator is told to restore from: {note}"
    );

    let none = newest_snapshot_note(None);
    assert!(
        none.contains("none yet"),
        "the first-run case must say so plainly instead of printing an empty path: {none}"
    );
}

/// Source with `//` line comments stripped, so a sentinel about a CALL SITE
/// cannot be tripped by prose that merely names the function.
///
/// Deliberately crude: a `//` inside a string literal (a URL) truncates that
/// line's code too. Harmless for the needles used here, and the alternative, a
/// real Rust parser, is more machinery than a wording pin deserves.
fn code_only(src: &str) -> String {
    src.lines()
        .map(|line| match line.split_once("//") {
            Some((code, _)) => code,
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The helpers are only worth having if the two non-TUI surfaces actually call
/// them. Behaviour tests prove the wording; these pins stop someone deleting the
/// call site and leaving the tests green over a blind daemon again.
#[test]
fn the_daemon_startup_and_doctor_both_wire_the_surface_up() {
    const UI: &str = include_str!("../cli/ui.rs");
    const DOCTOR: &str = include_str!("../cli/commands.rs");
    let ui = code_only(UI);
    let doctor = code_only(DOCTOR);

    assert!(
        ui.contains("db_integrity_failed_now"),
        "cmd_chat_inner must log the corruption verdict: a headless daemon has no \
         banner and was the exact blindness that cost the rpi5 cron rows their signal"
    );
    assert!(
        ui.contains("newest_snapshot_note"),
        "the daemon log line must name the file to restore from, not just the failure"
    );
    assert!(
        !ui.contains(".context(\"Failed to run database migrations\")"),
        "re-wrapping the refusal at the top of the chain puts the bare rpi5 masking \
         prefix back into what the operator reads"
    );

    assert!(
        doctor.contains("newest_snapshot_note"),
        "doctor must report the recovery asset"
    );
    assert!(
        doctor.contains("db_integrity_failed_now"),
        "doctor must report a post-migration integrity failure, not only a refusal"
    );
    // Non-consuming reads everywhere except the TUI: the flag SWAPS on read, so
    // two consuming consumers means one of them silently goes blind.
    assert!(
        !doctor.contains("db_integrity_failed()"),
        "doctor must peek, never consume: the TUI banner is the reader that needs \
         the flag"
    );
    assert!(
        !ui.contains("db_integrity_failed()"),
        "the startup path must peek, never consume: it runs before the TUI is built"
    );
}

/// `Path` is used only through the helpers; this keeps the import honest and
/// documents that the note is rendered from a real path, not a stringly-typed
/// approximation of one.
#[test]
fn the_note_is_rendered_from_a_path_not_a_name() {
    let dir = snapshot_dir_with(&[&format!("{PREFIX}33-20260928-030303")]);
    let path: &Path = dir.path();
    let newest = newest_snapshot(path).unwrap();
    assert!(
        newest.is_absolute(),
        "the restore instruction has to work from any cwd, so the reported path \
         must be absolute: {newest:?}"
    );
}
