-- Record the file revision in each database so a half-committed rewrite can be recovered.
ALTER TABLE projection_state ADD COLUMN source_id TEXT NOT NULL DEFAULT '';
ALTER TABLE projection_state ADD COLUMN source_size INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projection_state ADD COLUMN source_mtime TEXT NOT NULL DEFAULT '';

-- Guard lease updates when a second process takes over after expiry.
ALTER TABLE backfill_state ADD COLUMN lease_owner TEXT;

DROP INDEX idx_sessions_visible_created_at_ms;
DROP INDEX idx_sessions_visible_updated_at_ms;
DROP INDEX idx_sessions_visible_recency_at_ms;
DROP INDEX idx_sessions_interactive_created_at_ms;
DROP INDEX idx_sessions_interactive_updated_at_ms;
DROP INDEX idx_sessions_interactive_recency_at_ms;

-- Visible listing hides Sessions that never received a user message.
CREATE INDEX idx_sessions_visible_created_at_ms ON sessions(created_at_ms DESC, id DESC)
    WHERE has_user_message = 1;
CREATE INDEX idx_sessions_visible_updated_at_ms ON sessions(updated_at_ms DESC, id DESC)
    WHERE has_user_message = 1;
CREATE INDEX idx_sessions_visible_recency_at_ms ON sessions(recency_at_ms DESC, id DESC)
    WHERE has_user_message = 1;

-- Interactive listing additionally hides owned subagent Sessions.
CREATE INDEX idx_sessions_interactive_created_at_ms ON sessions(created_at_ms DESC, id DESC)
    WHERE has_user_message = 1 AND owner_session_id IS NULL;
CREATE INDEX idx_sessions_interactive_updated_at_ms ON sessions(updated_at_ms DESC, id DESC)
    WHERE has_user_message = 1 AND owner_session_id IS NULL;
CREATE INDEX idx_sessions_interactive_recency_at_ms ON sessions(recency_at_ms DESC, id DESC)
    WHERE has_user_message = 1 AND owner_session_id IS NULL;
