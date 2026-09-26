-- Audit trail READ + OUTCOME columns (#1705).
--
-- The pre-existing ACTION log (tool_executions) shows that a tool ran and
-- whether it errored. It does not show which retrievals entered the context
-- before a decision, nor whether the turn's final result was mechanically
-- verified. Cross-session postmortems therefore stop at correlation: a
-- failure that looks like a tool problem can actually be a bad READ (a
-- truncated file, a stale search hit) feeding an unverified conclusion.
--
-- turn_retrievals: one row per read-class retrieval (file read, search,
-- listing) that entered context. `kind` is the retrieval class
-- ('read' | 'search' | 'list'), `target` the path or query, `content_hash`
-- a sha256 of the content actually returned (so a later audit can tell a
-- stale read from a fresh one), and `preview` the first <=128 chars so the
-- /audit viewer can show WHAT was read without re-reading the file.
--
-- turn_outcomes: one row per turn message with the mechanically-observed
-- outcome ('verified' | 'failed' | 'unverified') plus the evidence string
-- (exit code, test-result line, or the reason no evidence existed).
-- Nothing here is model-judged: a verdict exists only when mechanical
-- evidence produced one.
--
-- Both tables are written ONLY when [features] audit_recording = true
-- (default off); until then they stay empty and /audit renders ACTION
-- rows only. Removal: turn the flag off (no code path touches the tables)
-- then
--   DROP TABLE IF EXISTS turn_retrievals;
--   DROP TABLE IF EXISTS turn_outcomes;
CREATE TABLE IF NOT EXISTS turn_retrievals (
    id           TEXT PRIMARY KEY NOT NULL,
    session_id   TEXT NOT NULL,
    message_id   TEXT NOT NULL,
    tool_name    TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('read', 'search', 'list')),
    target       TEXT NOT NULL,
    content_hash TEXT,
    preview      TEXT,
    created_at   INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_turn_retrievals_session
    ON turn_retrievals(session_id, created_at);

CREATE TABLE IF NOT EXISTS turn_outcomes (
    id         TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL,
    message_id TEXT NOT NULL UNIQUE,
    outcome    TEXT NOT NULL CHECK (outcome IN ('verified', 'failed', 'unverified')),
    evidence   TEXT,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_turn_outcomes_session
    ON turn_outcomes(session_id, created_at);
