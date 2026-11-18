-- History projection cursor for one Session file (codex `thread_history_projection_state`).
CREATE TABLE projection_state (
    session_id       TEXT PRIMARY KEY,
    path             TEXT NOT NULL,
    next_byte_offset INTEGER NOT NULL,
    next_sequence    INTEGER NOT NULL
);

-- One durable operation from operation_started to operation_finished (codex `thread_turns`).
CREATE TABLE runs (
    session_id               TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    run_id                   TEXT NOT NULL,
    lane                     TEXT NOT NULL,
    operation                TEXT NOT NULL,
    status                   TEXT NOT NULL,
    error_json               TEXT,
    start_sequence           INTEGER NOT NULL,
    start_byte_offset        INTEGER NOT NULL,
    end_sequence             INTEGER,
    end_byte_offset          INTEGER,
    started_at_ms            INTEGER NOT NULL,
    completed_at_ms          INTEGER,
    duration_ms              INTEGER,
    first_user_entry_id      TEXT,
    final_assistant_entry_id TEXT,
    PRIMARY KEY (session_id, run_id)
);
-- Runs of one Session in log order.
CREATE UNIQUE INDEX idx_runs_page ON runs(session_id, start_sequence);
-- Finished Runs paged by completion position.
CREATE INDEX idx_runs_end_page ON runs(session_id, end_sequence, run_id)
    WHERE end_sequence IS NOT NULL;

-- One transcript entry (codex `thread_items`) plus the tree columns clawcode needs.
CREATE TABLE entries (
    session_id      TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    entry_id        TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    lane            TEXT NOT NULL,
    parent_id       TEXT,
    run_id          TEXT,
    kind            TEXT NOT NULL,
    message_type    TEXT,
    created_at_ms   INTEGER NOT NULL,
    started_at_ms   INTEGER,
    completed_at_ms INTEGER,
    entry_json      TEXT NOT NULL,
    PRIMARY KEY (session_id, entry_id)
);
-- Whole-tree paging in log order (SessionTreeSnapshot without ACP).
CREATE UNIQUE INDEX idx_entries_page ON entries(session_id, sequence);
-- Entries of one Run in log order.
CREATE INDEX idx_entries_by_run_page ON entries(session_id, run_id, sequence);
-- User message navigation (codex `idx_thread_items_user_messages`).
CREATE INDEX idx_entries_user_messages ON entries(session_id, sequence)
    WHERE message_type IN ('user', 'expanded_user');

-- Current leaf of each lane, required to rebuild SessionTreeSnapshot.
CREATE TABLE lanes (
    session_id TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    lane       TEXT NOT NULL,
    leaf_id    TEXT,
    PRIMARY KEY (session_id, lane)
);
