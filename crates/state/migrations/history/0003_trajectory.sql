-- Every lane record mirrored verbatim, for queue marks, retries, and request contents.
CREATE TABLE records (
    session_id   TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    record_id    TEXT NOT NULL,
    sequence     INTEGER NOT NULL,
    lane         TEXT NOT NULL,
    run_id       TEXT,
    kind         TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    record_json  TEXT NOT NULL,
    PRIMARY KEY (session_id, record_id)
);
CREATE INDEX idx_records_by_run ON records(session_id, run_id, sequence);
CREATE INDEX idx_records_by_kind ON records(session_id, kind, sequence);

-- Request contents addressed by hash within one Session.
CREATE TABLE request_contents (
    session_id TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    hash       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    byte_len   INTEGER NOT NULL,
    content    TEXT NOT NULL,
    PRIMARY KEY (session_id, hash)
);
