//! Pre-migration image snapshot (#1779).
//!
//! The rpi5 incident: `/evolve` restarted the daemon, `run_migrations` wrote
//! `ALTER TABLE` against an already-damaged image, and the cron rows died
//! because the file being migrated was the only copy of itself. These tests pin
//! the two halves of the fix: the snapshot happens BEFORE any write, and a
//! snapshot that cannot be taken STOPS the migration.

use crate::config::profile::with_home_override_async;
use crate::db::Database;
use crate::db::migration_snapshot::{
    LATEST, PREFIX, RETENTION, rotate, snapshot_before_migrations,
};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// A file-backed database carrying one table and no migrations applied.
///
/// The canary table is what makes it "non-empty": the snapshot must contain it,
/// and must NOT contain anything a migration would have created.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("opencrabs.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute("CREATE TABLE canary(x TEXT)", []).unwrap();
    conn.execute("INSERT INTO canary VALUES ('nicole')", [])
        .unwrap();
    drop(conn);
    path
}

fn tables_in(path: &Path) -> Vec<String> {
    // READ_WRITE rather than READ_ONLY on purpose: the live database under test
    // is in WAL mode by the time these helpers run (the pool's post_create hook
    // applies the pragma), and a WAL database cannot be opened read-only without
    // its -shm sidecar. No CREATE flag, so a missing file is still a hard failure
    // instead of a silently empty result.
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows
}

fn dated_snapshots(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(PREFIX) && n != LATEST)
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- outcome

#[tokio::test]
async fn non_empty_db_is_snapshotted_before_migrations_run() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".opencrabs");
    std::fs::create_dir_all(&home).unwrap();
    let db_path = seeded_db(&home);

    let db = with_home_override_async(home.clone(), async {
        let db = Database::connect(&db_path).await.unwrap();
        db.run_migrations().await.unwrap();
        db
    })
    .await;
    let _ = db;

    let backups = home.join("backups");
    let snaps = dated_snapshots(&backups);
    assert_eq!(
        snaps.len(),
        1,
        "exactly one pre-migration snapshot expected"
    );

    // The ordering claim, measured rather than asserted from a log line: the
    // snapshot holds the canary and none of the migration's tables, so it was
    // taken before `to_latest` ran.
    let names = tables_in(&snaps[0]);
    assert!(
        names.contains(&"canary".to_string()),
        "snapshot must carry the pre-migration data, got {names:?}"
    );
    assert!(
        !names.contains(&"sessions".to_string()),
        "snapshot was taken AFTER migrations ran, not before: got {names:?}"
    );

    // The alias the refusal message promises must exist and match the newest.
    let latest = backups.join(LATEST);
    assert!(latest.exists(), "-latest alias must always name a file");
    assert_eq!(
        std::fs::read(&latest).unwrap().len(),
        std::fs::read(&snaps[0]).unwrap().len(),
        "-latest must be a copy of the newest snapshot"
    );
}

#[tokio::test]
async fn fresh_empty_db_skips_the_snapshot_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".opencrabs");
    std::fs::create_dir_all(&home).unwrap();
    let db_path = home.join("opencrabs.db");
    Connection::open(&db_path).unwrap(); // no tables at all

    with_home_override_async(home.clone(), async {
        let db = Database::connect(&db_path).await.unwrap();
        db.run_migrations().await.unwrap();
    })
    .await;

    let backups = home.join("backups");
    let snaps = if backups.exists() {
        dated_snapshots(&backups)
    } else {
        vec![]
    };
    assert!(
        snaps.is_empty(),
        "a brand-new database has nothing to protect: got {snaps:?}"
    );
}

/// The whole test suite calls `run_migrations()` on in-memory databases, and
/// `Connection::path()` strips a URI's query part, so `?mode=memory` is
/// invisible to it. Without the `PRAGMA database_list` check every `cargo test`
/// run would write 70 MB snapshots into the developer's real home.
#[tokio::test]
async fn in_memory_db_never_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".opencrabs");
    std::fs::create_dir_all(&home).unwrap();

    with_home_override_async(home.clone(), async {
        let db = Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
    })
    .await;

    assert!(
        !home.join("backups").exists(),
        "in-memory database must not create a backups dir"
    );
}

// ---------------------------------------------------------------- retention

#[tokio::test]
async fn rotation_keeps_seven_dated_copies_and_the_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = seeded_db(tmp.path());
    let backups = tmp.path().join("backups");
    let conn = Connection::open(&db_path).unwrap();

    // Nine startups. Same-second collisions are expected in a crash-restart
    // loop, so the names must stay distinct rather than fail.
    for _ in 0..9 {
        snapshot_before_migrations(&conn, &backups)
            .unwrap()
            .expect("non-empty db must snapshot");
    }

    let snaps = dated_snapshots(&backups);
    assert_eq!(
        snaps.len(),
        RETENTION,
        "retention is the directed 7-day window, got {}",
        snaps.len()
    );
    assert!(
        backups.join(LATEST).exists(),
        "rotation must never prune the alias"
    );

    // Oldest first, so the survivors are the newest RETENTION.
    let removed = rotate(&backups).unwrap();
    assert_eq!(removed, 0, "already at the cap, nothing to prune");
    assert_eq!(dated_snapshots(&backups).len(), RETENTION);
}

// ------------------------------------------------------------ refusal gate

#[tokio::test]
async fn migrations_are_refused_when_the_snapshot_cannot_be_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".opencrabs");
    std::fs::create_dir_all(&home).unwrap();
    let db_path = seeded_db(&home);

    // Sabotage the destination: a regular FILE where the backups directory has
    // to be, so create_dir_all fails and VACUUM INTO never gets a chance.
    std::fs::write(home.join("backups"), b"not a directory").unwrap();

    let (before, err) = with_home_override_async(home.clone(), async {
        let db = Database::connect(&db_path).await.unwrap();
        // Baseline AFTER the pool's first real connection, not before it.
        // connect() is lazy: the first get() runs post_create, whose
        // journal_mode=WAL rewrites the image header (bytes 18-19 go 1,1 -> 2,2)
        // and bumps the change counter. That is SQLite converting the file to
        // WAL, it happens on every single open, and it is not a migration.
        // Measuring from the pre-open bytes would fail on the pragma and prove
        // nothing about the refusal.
        let _warm = db.pool.get().await.unwrap();
        let before = std::fs::read(&db_path).unwrap();
        let err = db.run_migrations().await.unwrap_err();
        (before, err)
    })
    .await;

    let msg = format!("{err:#}");
    assert!(
        msg.contains("Refusing to run database migrations"),
        "message must say it refused, got: {msg}"
    );
    assert!(
        !msg.contains("Failed to run database migrations"),
        "the misleading migration wording must not resurface here: {msg}"
    );
    assert!(
        msg.contains("Restore it by copying a snapshot"),
        "message must name the recovery action: {msg}"
    );
    assert!(
        msg.contains("brain files and config are untouched"),
        "message must scope the blast radius: {msg}"
    );

    // Nothing was written past the pragma. This is the zero-data-loss half of
    // the promise: a refused migration leaves the image byte-identical, so it
    // stays repairable by the rescue tooling instead of being half-migrated.
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        before,
        "refused migration must not touch the database file"
    );
    let names = tables_in(&db_path);
    assert!(
        !names.contains(&"sessions".to_string()),
        "migrations must NOT have run: got {names:?}"
    );

    // And the row that mattered is still there, still readable, still at
    // user_version 0. The byte comparison above can only ever prove "unchanged
    // since the baseline"; this proves "the data survives the refusal".
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        version, 0,
        "a refused migration must not advance user_version"
    );
    let canary: String = conn
        .query_row("SELECT x FROM canary", [], |r| r.get(0))
        .unwrap();
    assert_eq!(canary, "nicole", "the pre-existing row must survive refusal");
}
