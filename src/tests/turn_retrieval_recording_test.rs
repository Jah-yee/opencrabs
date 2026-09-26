//! Tests for the audit-trail READ + OUTCOME recording (#1705).
//!
//! Pins the pure classification seams (retrieval kind, target extraction,
//! mechanical turn-outcome verdicts, viewer render modes) and the SQL
//! round-trip (retrieval + outcome rows join onto ACTION rows through the
//! message_id). The recording write path is gated by
//! `[features] audit_recording` before any DB work; the gate itself is a
//! bool read at the two call sites, so these tests pin everything the gate
//! guards.

use crate::channels::commands::render_audit_rows;
use crate::db::repository::turn_retrieval::{
    AuditRow, TurnRetrievalRepository, audit_target, retrieval_kind, turn_outcome_from_outputs,
};
use serde_json::json;

async fn make_db() -> crate::db::Database {
    let db = crate::db::Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db
}

#[test]
fn retrieval_kind_maps_read_class_tools() {
    assert_eq!(retrieval_kind("read_file"), Some("read"));
    assert_eq!(retrieval_kind("load_brain_file"), Some("read"));
    assert_eq!(retrieval_kind("grep"), Some("search"));
    assert_eq!(retrieval_kind("web_search"), Some("search"));
    assert_eq!(retrieval_kind("memory_search"), Some("search"));
    assert_eq!(retrieval_kind("ls"), Some("list"));
}

#[test]
fn retrieval_kind_rejects_non_retrieval_tools() {
    // Writers and shell never pull external content into context on their
    // own; they must not produce READ rows.
    assert_eq!(retrieval_kind("write_file"), None);
    assert_eq!(retrieval_kind("edit_file"), None);
    assert_eq!(retrieval_kind("bash"), None);
    assert_eq!(retrieval_kind("plan"), None);
    assert_eq!(retrieval_kind(""), None);
}

#[test]
fn audit_target_prefers_path_then_query_then_pattern() {
    assert_eq!(
        audit_target("read_file", &json!({"path": "/tmp/x.md"})),
        Some("/tmp/x.md".to_string())
    );
    assert_eq!(
        audit_target("web_search", &json!({"query": "rust async"})),
        Some("rust async".to_string())
    );
    assert_eq!(
        audit_target("grep", &json!({"pattern": "fn main", "path": "/src"})),
        Some("/src".to_string()),
        "path wins over pattern when both exist"
    );
    assert_eq!(
        audit_target("grep", &json!({"pattern": "fn main"})),
        Some("fn main".to_string())
    );
}

#[test]
fn audit_target_skips_blank_values() {
    assert_eq!(audit_target("read_file", &json!({"path": "   "})), None);
    assert_eq!(audit_target("bash", &json!({"command": "ls"})), None);
}

fn row(message_id: &str, tool: &str, status: &str) -> AuditRow {
    AuditRow {
        session_id: "sess".to_string(),
        message_id: message_id.to_string(),
        tool_name: tool.to_string(),
        status: status.to_string(),
        created_at: 0,
        read_count: 0,
        last_read: None,
        outcome: None,
    }
}

#[test]
fn viewer_off_mode_hints_the_config_key() {
    let rows = vec![row("abcdefgh1234", "read_file", "success")];
    let out = render_audit_rows(&rows, false);
    assert!(
        out.contains("audit_recording"),
        "OFF header names the config key: {out}"
    );
    assert!(out.contains("TURN|ACTION|READ|OUTCOME"), "columns: {out}");
    assert!(out.contains("abcdefgh|read_file|-|-"), "row: {out}");
}

#[test]
fn viewer_on_mode_renders_reads_and_outcomes() {
    let mut r = row("abcdefgh1234", "grep", "success");
    r.read_count = 2;
    r.last_read = Some("search:fn main".to_string());
    r.outcome = Some("verified".to_string());
    let rows = vec![r];
    let out = render_audit_rows(&rows, true);
    assert!(out.contains("READ+OUTCOME recorded"), "ON header: {out}");
    assert!(
        out.contains("abcdefgh|grep|2: search:fn main|verified"),
        "row carries count, newest read, verdict: {out}"
    );
}

#[test]
fn viewer_marks_errored_actions() {
    let rows = vec![row("abcdefgh1234", "bash", "error")];
    let out = render_audit_rows(&rows, true);
    assert!(out.contains("bash (err)"), "error status marked: {out}");
}

#[test]
fn outcome_classifier_verifies_on_green_receipt() {
    let outputs =
        vec!["running tests...\ntest result: ok. 12 passed; 0 failed; 0 ignored".to_string()];
    let (outcome, evidence) = turn_outcome_from_outputs(&outputs);
    assert_eq!(outcome, "verified");
    assert!(
        evidence
            .as_deref()
            .unwrap_or_default()
            .contains("12 passed"),
        "evidence quotes the receipt: {evidence:?}"
    );
}

#[test]
fn outcome_classifier_fails_on_red_receipt() {
    let outputs = vec!["test result: FAILED. 5 passed; 1 failed; 0 ignored".to_string()];
    let (outcome, evidence) = turn_outcome_from_outputs(&outputs);
    assert_eq!(outcome, "failed");
    assert!(
        evidence.is_some(),
        "failed verdict carries the receipt line"
    );
}

#[test]
fn outcome_classifier_fails_on_rustc_error() {
    let outputs = vec!["error[E0308]: mismatched types".to_string()];
    let (outcome, _) = turn_outcome_from_outputs(&outputs);
    assert_eq!(outcome, "failed");
}

#[test]
fn outcome_classifier_reports_unverified_without_signals() {
    let outputs = vec!["file contents here".to_string(), "no receipts".to_string()];
    let (outcome, evidence) = turn_outcome_from_outputs(&outputs);
    assert_eq!(outcome, "unverified");
    assert_eq!(evidence, None, "no invented evidence");
}

#[test]
fn outcome_classifier_red_wins_over_green() {
    let outputs = vec![
        "test result: ok. 3 passed; 0 failed".to_string(),
        "test result: FAILED. 1 passed; 2 failed".to_string(),
    ];
    let (outcome, _) = turn_outcome_from_outputs(&outputs);
    assert_eq!(outcome, "failed", "a red receipt anywhere fails the turn");
}

#[tokio::test]
async fn roundtrip_retrieval_and_outcome_join_action_rows() {
    let db = make_db().await;
    let repo = TurnRetrievalRepository::new(db.pool().clone());
    let actions = crate::db::repository::ToolExecutionRepository::new(db.pool().clone());

    // The turn's ACTION row (always written, regardless of the flag).
    actions
        .record(
            "act-1",
            "msg-1",
            "sess-1",
            "read_file",
            "success",
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // Recording on: the READ row and the turn's OUTCOME verdict.
    repo.record_retrieval(
        "ret-1",
        "sess-1",
        "msg-1",
        "read_file",
        "read",
        "/tmp/notes.md",
        Some("abc123"),
        Some("hello world"),
    )
    .await
    .unwrap();
    repo.record_outcome(
        "out-1",
        "sess-1",
        "msg-1",
        "verified",
        Some("test result: ok. 1 passed; 0 failed"),
    )
    .await
    .unwrap();

    let rows = repo.recent_audit_rows(10).await.unwrap();
    assert_eq!(rows.len(), 1, "one ACTION row: {rows:?}");
    assert_eq!(rows[0].read_count, 1);
    assert_eq!(
        rows[0].last_read.as_deref(),
        Some("read:/tmp/notes.md"),
        "newest retrieval rendered as kind:target"
    );
    assert_eq!(rows[0].outcome.as_deref(), Some("verified"));
}

#[tokio::test]
async fn roundtrip_without_recording_still_lists_actions() {
    let db = make_db().await;
    let repo = TurnRetrievalRepository::new(db.pool().clone());
    let actions = crate::db::repository::ToolExecutionRepository::new(db.pool().clone());

    actions
        .record(
            "act-2", "msg-2", "sess-1", "bash", "error", None, None, None,
        )
        .await
        .unwrap();

    let rows = repo.recent_audit_rows(10).await.unwrap();
    assert_eq!(rows.len(), 1, "ACTION rows exist with recording off");
    assert_eq!(rows[0].read_count, 0, "no READ rows were written");
    assert_eq!(rows[0].outcome, None, "no OUTCOME rows were written");
}

#[tokio::test]
async fn outcome_replaces_earlier_verdict_for_same_turn() {
    let db = make_db().await;
    let repo = TurnRetrievalRepository::new(db.pool().clone());

    repo.record_outcome("out-a", "sess-1", "msg-1", "unverified", None)
        .await
        .unwrap();
    repo.record_outcome("out-b", "sess-1", "msg-1", "verified", Some("green"))
        .await
        .unwrap();

    // The viewer starts from tool_executions, so count the outcome table
    // directly: one row per turn message, and the FINAL verdict wins.
    let verdicts: Vec<String> = db
        .pool()
        .get()
        .await
        .unwrap()
        .interact(
            move |conn| -> std::result::Result<Vec<String>, rusqlite::Error> {
                let mut stmt =
                    conn.prepare("SELECT outcome FROM turn_outcomes WHERE message_id = 'msg-1'")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<String>, rusqlite::Error>>()?;
                Ok(rows)
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        verdicts,
        vec!["verified".to_string()],
        "INSERT OR REPLACE keeps one FINAL verdict per turn message"
    );
}

#[tokio::test]
async fn retrieval_preview_is_capped_at_128_chars() {
    let db = make_db().await;
    let repo = TurnRetrievalRepository::new(db.pool().clone());
    let actions = crate::db::repository::ToolExecutionRepository::new(db.pool().clone());
    actions
        .record(
            "act-3",
            "msg-3",
            "sess-1",
            "read_file",
            "success",
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let long_preview = "x".repeat(500);
    repo.record_retrieval(
        "ret-2",
        "sess-1",
        "msg-3",
        "read_file",
        "read",
        "/tmp/big.md",
        None,
        Some(&long_preview),
    )
    .await
    .unwrap();

    let count: i64 = db
        .pool()
        .get()
        .await
        .unwrap()
        .interact(move |conn| {
            conn.query_row(
                "SELECT length(preview) FROM turn_retrievals WHERE id = 'ret-2'",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        count, 128,
        "preview capped at the migration's documented cap"
    );
}

#[tokio::test]
async fn kind_check_constraint_rejects_unknown_class() {
    let db = make_db().await;
    let repo = TurnRetrievalRepository::new(db.pool().clone());
    let result = repo
        .record_retrieval(
            "ret-3",
            "sess-1",
            "msg-4",
            "read_file",
            "nonsense",
            "/x",
            None,
            None,
        )
        .await;
    assert!(result.is_err(), "CHECK (kind IN ...) must hold");
}
