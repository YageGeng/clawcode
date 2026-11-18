-- Session metadata mirrored from JSONL logs (codex `threads`); every column is rebuildable.
CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    path              TEXT NOT NULL,
    cwd               TEXT NOT NULL,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    recency_at_ms     INTEGER NOT NULL,
    name              TEXT,
    preview           TEXT NOT NULL DEFAULT '',
    has_user_message  INTEGER NOT NULL DEFAULT 0 CHECK (has_user_message IN (0, 1)),
    provider_id       TEXT,
    model_id          TEXT,
    thinking_level    TEXT,
    session_mode      TEXT NOT NULL DEFAULT 'default',
    tokens_used       INTEGER NOT NULL DEFAULT 0,
    parent_session_id TEXT,
    owner_session_id  TEXT,
    agent_name        TEXT,
    agent_nickname    TEXT,
    git_sha           TEXT,
    git_branch        TEXT,
    git_origin_url    TEXT,
    app_version       TEXT,
    -- Ownership is all-or-nothing in the header.
    CHECK ((owner_session_id IS NULL) = (agent_name IS NULL))
);

-- Global listing, one index per codex sort key.
CREATE INDEX idx_sessions_created_at_ms ON sessions(created_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_updated_at_ms ON sessions(updated_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_recency_at_ms ON sessions(recency_at_ms DESC, id DESC);

-- Workspace listing for session/list with cwd and kernel workspace lookups.
CREATE INDEX idx_sessions_cwd_created_at_ms ON sessions(cwd, created_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_cwd_updated_at_ms ON sessions(cwd, updated_at_ms DESC, id DESC);
CREATE INDEX idx_sessions_cwd_recency_at_ms ON sessions(cwd, recency_at_ms DESC, id DESC);

-- Visible listing hides Sessions that never received a user message.
CREATE INDEX idx_sessions_visible_created_at_ms ON sessions(created_at_ms DESC, id DESC)
    WHERE preview <> '';
CREATE INDEX idx_sessions_visible_updated_at_ms ON sessions(updated_at_ms DESC, id DESC)
    WHERE preview <> '';
CREATE INDEX idx_sessions_visible_recency_at_ms ON sessions(recency_at_ms DESC, id DESC)
    WHERE preview <> '';

-- Interactive listing additionally hides owned subagent Sessions.
CREATE INDEX idx_sessions_interactive_created_at_ms ON sessions(created_at_ms DESC, id DESC)
    WHERE preview <> '' AND owner_session_id IS NULL;
CREATE INDEX idx_sessions_interactive_updated_at_ms ON sessions(updated_at_ms DESC, id DESC)
    WHERE preview <> '' AND owner_session_id IS NULL;
CREATE INDEX idx_sessions_interactive_recency_at_ms ON sessions(recency_at_ms DESC, id DESC)
    WHERE preview <> '' AND owner_session_id IS NULL;

-- Owned children: close cascades, rosters, and mail delivery by agent name.
CREATE INDEX idx_sessions_owner ON sessions(owner_session_id, agent_name)
    WHERE owner_session_id IS NOT NULL;

-- Provider filter.
CREATE INDEX idx_sessions_provider ON sessions(provider_id);

-- Metadata projection cursor, stored with the rows it advances.
CREATE TABLE projection_state (
    session_id       TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    next_byte_offset INTEGER NOT NULL,
    next_sequence    INTEGER NOT NULL
);

-- One-time filesystem backfill with lease and resumable watermark (codex `backfill_state`).
CREATE TABLE backfill_state (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    status             TEXT NOT NULL CHECK (status IN ('pending', 'running', 'complete')),
    last_watermark     TEXT,
    last_success_at_ms INTEGER,
    updated_at_ms      INTEGER NOT NULL
);
INSERT INTO backfill_state (id, status, updated_at_ms)
VALUES (1, 'pending', CAST(strftime('%s', 'now') AS INTEGER) * 1000);
