-- Record the file revision in each database so a half-committed rewrite can be recovered.
ALTER TABLE projection_state ADD COLUMN source_id TEXT NOT NULL DEFAULT '';
ALTER TABLE projection_state ADD COLUMN source_size INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projection_state ADD COLUMN source_mtime TEXT NOT NULL DEFAULT '';

-- Find the newest open run on a lane without visiting completed runs.
CREATE INDEX idx_runs_open_lane ON runs(session_id, lane, start_sequence DESC)
    WHERE status = 'in_progress';
