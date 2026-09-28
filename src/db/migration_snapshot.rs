//! Pre-migration image snapshot (#1779).
//!
//! A migration is a *write*. When the image already carries damage, the
//! migration turns a readable-but-broken database into a destroyed one: the
//! rpi5 incident (arfonzo, 2026-09-27) lost every cron row because `/evolve`
//! restarted the daemon, `run_migrations` ran `ALTER TABLE` against a torn
//! page 1, and the only copy of the data was the file being overwritten.
//!
//! So: copy the whole image *before* anything touches it, and refuse to
//! migrate when that copy cannot be made. A failed snapshot on a non-empty
//! database means the image is either unreadable or the disk is unusable, and
//! both cases migrating is the worst possible next action.
//!
//! Retention is 7 dated copies (owner directive 2026-09-28: "rolling 7 days"),
//! plus one stable `-latest` name so a message, a script or a panicked user can
//! always point at exactly one file instead of globbing for the newest.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Dated snapshots kept before the oldest is pruned.
pub const RETENTION: usize = 7;

/// Prefix shared by every snapshot name, so rotation can recognise its own.
pub const PREFIX: &str = "opencrabs.db.pre-migration-";

/// Stable alias pointing at the newest snapshot. Never pruned by rotation.
pub const LATEST: &str = "opencrabs.db.pre-migration-latest";

/// Where snapshots for the active profile are written.
///
/// `crate::config::opencrabs_home()` (not the `types::io` path) because
/// `config::types` is `pub(crate)` with a re-export; every other caller in the
/// crate goes through the re-export, and so does this.
pub fn snapshot_dir() -> PathBuf {
    crate::config::opencrabs_home().join("backups")
}

/// The file a connection is really backed by, or `None` for an in-memory DB.
///
/// `Connection::path()` cannot be used here: it wraps `sqlite3_db_filename`,
/// which strips the query part of a URI, so the `?mode=memory` URIs the test
/// suite connects with would never be recognised and every unit test would
/// write snapshots into the developer's real home.
///
/// `PRAGMA database_list` is the canonical signal: measured on
/// `sqlite3 "file:mem_abc123?mode=memory&cache=shared" "pragma database_list"`,
/// the filename column comes back EMPTY, same as for a plain `:memory:`. The
/// `is_file()` gate is defence in depth for builds whose SQLite reports the
/// memory-db name there instead, and costs one stat.
fn backing_file(conn: &Connection) -> Result<Option<String>> {
    let file: String = conn
        .query_row("PRAGMA database_list", [], |r| r.get(2))
        .context("read PRAGMA database_list")?;
    if file.is_empty() || file == ":memory:" {
        return Ok(None);
    }
    if !Path::new(&file).is_file() {
        return Ok(None);
    }
    Ok(Some(file))
}

/// True when there is nothing on disk worth keeping.
///
/// Two distinct cases: an in-memory database has no file to copy at all, and a
/// brand-new file has no user tables. Both skip *silently* rather than fail.
fn is_empty_or_transient(conn: &Connection) -> Result<bool> {
    if backing_file(conn)?.is_none() {
        return Ok(true);
    }
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .context("count sqlite_master rows")?;
    Ok(tables == 0)
}

/// Copy the whole image into `dir` before any migration touches it.
///
/// Returns the dated snapshot path, or `None` when the database is empty or
/// in-memory and there is nothing to protect. Errors are fatal to the caller:
/// a non-empty database that cannot be snapshotted must NOT be migrated.
pub fn snapshot_before_migrations(conn: &Connection, dir: &Path) -> Result<Option<PathBuf>> {
    if is_empty_or_transient(conn)? {
        return Ok(None);
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("create backups dir {}", dir.display()))?;

    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .context("read user_version")?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

    // VACUUM INTO refuses to overwrite, so a same-second second attempt needs a
    // distinct name rather than a failure. Two startups inside one second are
    // rare, but a crash-restart loop makes them routine.
    let mut dated = dir.join(format!("{PREFIX}{user_version}-{stamp}"));
    for n in 2.. {
        if !dated.exists() {
            break;
        }
        dated = dir.join(format!("{PREFIX}{user_version}-{stamp}-{n}"));
    }

    // VACUUM INTO is the only copy that is safe against a live writer: it
    // produces a compact, integrity-checked image in one transaction, where a
    // plain file copy of a WAL database can catch a half-written page.
    conn.execute("VACUUM INTO ?1", [&dated.to_string_lossy()])
        .with_context(|| format!("VACUUM INTO {}", dated.display()))?;

    // Stable alias for the newest snapshot. A copy, not a rename: the dated file
    // is the retention unit and must stay put.
    let latest = dir.join(LATEST);
    if let Err(e) = std::fs::copy(&dated, &latest) {
        tracing::warn!(
            "Snapshot {} written, but the -latest alias could not be updated: {e}",
            dated.display()
        );
    }

    rotate(dir)?;
    Ok(Some(dated))
}

/// Prune dated snapshots beyond the newest [`RETENTION`].
///
/// Never touches the `-latest` alias, and never fails the caller over a prune:
/// the snapshot is already safe, and a cleanup problem is not a reason to
/// refuse a migration.
pub fn rotate(dir: &Path) -> Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    let mut dated: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(PREFIX) && n != LATEST)
                .unwrap_or(false)
        })
        .collect();
    // Names embed a sortable timestamp, so lexical order is chronological order.
    dated.sort();
    let mut removed = 0;
    if dated.len() > RETENTION {
        for old in &dated[..dated.len() - RETENTION] {
            match std::fs::remove_file(old) {
                Ok(_) => removed += 1,
                Err(e) => {
                    tracing::warn!("Could not prune stale snapshot {}: {e}", old.display())
                }
            }
        }
    }
    Ok(removed)
}

/// The refusal an operator acts on, worded for the stage that found the damage.
///
/// Explains what to do next, not only what broke.
///
/// `stage` is spelled out per call site instead of sharing one generic phrase
/// because the two stages fail for different reasons, and the reader is usually
/// panicked and reading over SSH: "the pre-migration integrity check failed"
/// says the image was already broken, "the pre-migration snapshot failed" says
/// the copy could not be made. Collapsing them into "something failed" is what
/// made the original rpi5 receipt useless two days into the incident.
fn refuse(stage: &str, dir: &Path, cause: &str) -> String {
    let latest = dir.join(LATEST);
    let hint = if latest.exists() {
        format!(
            "An earlier snapshot is still available at {}.",
            latest.display()
        )
    } else {
        format!("No snapshot exists in {} yet.", dir.display())
    };
    format!(
        "Refusing to run database migrations: {stage} ({cause}). \
         {hint} Nothing has been written to the database. \
         Restore it by copying a snapshot over the database file, or repair the header, \
         then start again. Your brain files and config are untouched."
    )
}

/// Refusal for a snapshot that could not be made.
pub fn refusal_message(dir: &Path, cause: &str) -> String {
    refuse("the pre-migration snapshot failed", dir, cause)
}

/// Guard used by `run_migrations`. `Ok(())` means "safe to migrate".
///
/// `dir` is resolved by the CALLER, on its own task, and never here:
/// `opencrabs_home()` reads a task-local profile override, and this runs inside
/// an `interact` closure on a blocking thread where that override is not set, so
/// resolving it in here would silently write snapshots into the default
/// profile's home no matter which profile is starting up.
pub fn guard(conn: &Connection, dir: &Path) -> Result<()> {
    match snapshot_before_migrations(conn, dir) {
        Ok(Some(p)) => {
            tracing::info!("Pre-migration snapshot: {}", p.display());
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => bail!("{}", refusal_message(dir, &e.to_string())),
    }
}

/// Check the image before any migration can write to it (#1779 defect 2).
///
/// Deliberately NOT routed through the pool. `post_create` applies
/// `PRAGMA journal_mode = WAL`, which is a *write*, so on a torn image the pool
/// cannot produce a connection at all, and a check that needs one is unreachable
/// in the exact incident it exists to catch. The rpi5 receipt is literally
/// "post_create hook failed: database disk image is malformed", emitted from
/// `pool.get()` before a single line of migration code ran. So this opens its
/// own read-write connection to the file instead.
///
/// `integrity_check` rather than `quick_check`: quick_check skips the page-ref
/// and freelist walks, and a torn page 1 breaks precisely those. The cost is the
/// same one the post-migration check already paid on every startup.
///
/// A damaged image reports as an `Err` from the pragma, not as a row that says
/// so. Verified 2026-09-28 against a real file: blanking bytes 100..200 (the
/// page-1 b-tree header) leaves the magic intact and makes `integrity_check`
/// fail with SQLITE_CORRUPT instead of returning text, so `Err` is a verdict
/// here and must refuse, never escape as if the plumbing broke.
pub fn integrity_preflight(path: &str, dir: &Path) -> Result<()> {
    let cause = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        Err(e) => format!("the database could not be opened ({e})"),
        Ok(conn) => {
            match conn.pragma_query_value(None, "integrity_check", |row| row.get::<_, String>(0)) {
                Ok(report) if report == "ok" => return Ok(()),
                Ok(report) => format!("integrity_check reported {report:?}"),
                Err(e) => format!("integrity_check could not run ({e})"),
            }
        }
    };
    bail!(
        "{}",
        refuse("the pre-migration integrity check failed", dir, &cause)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The alias must never match the dated-name filter, or rotation would
    /// delete the one file the refusal message promises.
    #[test]
    fn latest_alias_is_not_a_dated_name() {
        assert!(LATEST.starts_with(PREFIX));
        assert!(
            LATEST
                .trim_start_matches(PREFIX)
                .chars()
                .all(|c| !c.is_ascii_digit()),
            "alias must carry no digits so it is distinguishable from dated copies"
        );
    }

    #[test]
    fn retention_is_the_directed_window() {
        assert_eq!(RETENTION, 7, "owner directive 2026-09-28: rolling 7 days");
    }
}
