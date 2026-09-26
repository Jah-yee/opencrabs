//! Turn Retrieval Repository (#1705)
//!
//! The audit trail's READ and OUTCOME columns. `tool_executions` (ACTION)
//! proves a tool ran; these tables prove what entered the context before a
//! decision and whether the turn's result was mechanically verified.
//!
//! Both tables stay empty unless `[features] audit_recording = true`; the
//! write sites gate on the flag BEFORE any DB work, so an off flag costs
//! nothing per tool call.

use crate::db::Pool;
use crate::db::database::interact_err;
use anyhow::{Context as _, Result};
use rusqlite::params;

/// One audit-viewer row: an ACTION with whatever READ/OUTCOME data exists
/// for its turn message (`None` when recording was off or the turn had no
/// read-class tool calls).
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub session_id: String,
    pub message_id: String,
    pub tool_name: String,
    pub status: String,
    pub created_at: i64,
    /// Number of recorded retrievals for the turn message.
    pub read_count: i64,
    /// Most recent retrieval as `kind:target` (newest first), if any.
    pub last_read: Option<String>,
    /// Mechanically-observed outcome verdict, if recorded.
    pub outcome: Option<String>,
}

/// Repository for the audit READ + OUTCOME tables (#1705).
#[derive(Clone)]
pub struct TurnRetrievalRepository {
    pool: Pool,
}

/// The retrieval class of a read-class tool, or `None` when the tool does
/// not put external content into context (write, edit, shell, ...) and so
/// has no READ row. Pure name mapping: no fs access, so the classification
/// is testable without a harness.
pub fn retrieval_kind(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "read_file" | "load_brain_file" | "analyze_image" | "analyze_video" => Some("read"),
        "grep" | "glob" | "web_search" | "exa_search" | "memory_search" | "session_search" => {
            Some("search")
        }
        "ls" => Some("list"),
        _ => None,
    }
}

/// The `target` recorded for a read-class call: the path it read/listed or
/// the query it ran, pulled from the tool's own input by key preference
/// (`path`, then `query`, then `pattern`). Pure JSON access, testable
/// without a harness.
pub fn audit_target(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    for key in ["path", "query", "pattern"] {
        if let Some(s) = input.get(key).and_then(|v| v.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.to_string());
        }
    }
    tracing::debug!("[AUDIT] no target key for read-class tool {tool_name}");
    None
}

/// Mechanically classify a settled turn from its tool outputs (#1705).
/// Nothing here is model-judged: a verdict exists only when a mechanical
/// signal produced one.
///
/// - any `test result:` line with a non-zero failed count, or a rustc
///   `error[E...]` compile error → `("failed", that line)`
/// - otherwise at least one `test result:` line (all zero-failed) →
///   `("verified", that line)`
/// - none of the above → `("unverified", None)`: tools ran but nothing in
///   the outputs proves or disproves the result
pub fn turn_outcome_from_outputs(outputs: &[String]) -> (&'static str, Option<String>) {
    let mut verified_line: Option<String> = None;
    for out in outputs {
        for line in out.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("test result: ") {
                let failed = rest
                    .split(';')
                    .filter_map(|seg| {
                        let seg = seg.trim();
                        seg.strip_suffix(" failed")
                            .and_then(|n| n.trim().parse::<u64>().ok())
                    })
                    .sum::<u64>();
                if failed > 0 {
                    return ("failed", Some(shorten(trimmed)));
                }
                if verified_line.is_none() {
                    verified_line = Some(shorten(trimmed));
                }
            } else if trimmed.starts_with("error[E") {
                return ("failed", Some(shorten(trimmed)));
            }
        }
    }
    match verified_line {
        Some(line) => ("verified", Some(line)),
        None => ("unverified", None),
    }
}

fn shorten(s: &str) -> String {
    s.chars().take(200).collect()
}

impl TurnRetrievalRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Persist one READ row (one per read-class tool call). `preview` is
    /// capped to 128 chars by the caller; cap again here so a caller bug
    /// cannot bloat the table.
    #[allow(clippy::too_many_arguments)] // one arg per column, house pattern
    pub async fn record_retrieval(
        &self,
        id: &str,
        session_id: &str,
        message_id: &str,
        tool_name: &str,
        kind: &str,
        target: &str,
        content_hash: Option<&str>,
        preview: Option<&str>,
    ) -> Result<()> {
        let preview = preview.map(|p| p.chars().take(128).collect::<String>());
        let id = id.to_string();
        let session_id = session_id.to_string();
        let message_id = message_id.to_string();
        let tool_name = tool_name.to_string();
        let kind = kind.to_string();
        let target = target.to_string();
        let content_hash = content_hash.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO turn_retrievals \
                     (id, session_id, message_id, tool_name, kind, target, content_hash, preview) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        session_id,
                        message_id,
                        tool_name,
                        kind,
                        target,
                        content_hash,
                        preview
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to record turn retrieval")?;
        Ok(())
    }

    /// Persist the turn's OUTCOME row. One per turn message (UNIQUE
    /// `message_id`); a later call replaces the earlier verdict, so the
    /// table always holds the FINAL mechanical answer for the turn.
    pub async fn record_outcome(
        &self,
        id: &str,
        session_id: &str,
        message_id: &str,
        outcome: &str,
        evidence: Option<&str>,
    ) -> Result<()> {
        let id = id.to_string();
        let session_id = session_id.to_string();
        let message_id = message_id.to_string();
        let outcome = outcome.to_string();
        let evidence = evidence.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO turn_outcomes \
                     (id, session_id, message_id, outcome, evidence) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, session_id, message_id, outcome, evidence],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to record turn outcome")?;
        Ok(())
    }

    /// The `/audit` viewer's rows: the last `limit` ACTION entries joined
    /// with the READ count / newest READ and the OUTCOME verdict for their
    /// turn messages. Works with recording OFF (READ/OUTCOME fields just
    /// stay `None`/0) because it starts from `tool_executions`.
    pub async fn recent_audit_rows(&self, limit: u32) -> Result<Vec<AuditRow>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| -> anyhow::Result<Vec<AuditRow>> {
                let mut stmt = conn.prepare(
                    "SELECT t.session_id, t.message_id, t.tool_name, t.status, t.created_at, \
                     (SELECT COUNT(*) FROM turn_retrievals r WHERE r.message_id = t.message_id), \
                     (SELECT r.kind || ':' || r.target FROM turn_retrievals r \
                      WHERE r.message_id = t.message_id ORDER BY r.created_at DESC LIMIT 1), \
                     o.outcome \
                     FROM tool_executions t \
                     LEFT JOIN turn_outcomes o ON o.message_id = t.message_id \
                     ORDER BY t.created_at DESC, t.id DESC LIMIT ?1",
                )?;
                let rows = stmt
                    .query_map(params![limit], |row| {
                        Ok(AuditRow {
                            session_id: row.get(0)?,
                            message_id: row.get(1)?,
                            tool_name: row.get(2)?,
                            status: row.get(3)?,
                            created_at: row.get(4)?,
                            read_count: row.get(5)?,
                            last_read: row.get(6)?,
                            outcome: row.get(7)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<AuditRow>, rusqlite::Error>>()?;
                Ok(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to load audit rows")
    }
}
