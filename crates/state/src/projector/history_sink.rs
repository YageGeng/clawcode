//! Applies log lines to the runs, entries, and lanes of one Session (spec §3.1, history rules).

use protocol::{EntryId, LaneId, MessageTiming, SessionId};
use sqlx::types::Json;
use sqlx::{Acquire, Sqlite, SqliteConnection, Transaction};
use store::{
    LogMutation, LoggedMutation, RecordKind, SessionEntry, SessionRecord,
};

use super::{Cursor, HISTORY_POSITION, Position, SourceRevision};
use crate::StateError;

/// Status of a Run whose `operation_finished` record has not been read yet.
const IN_PROGRESS: &str = "in_progress";

/// Selects the latest active run using only the partial index for this lane.
const OPEN_RUN_SQL: &str = "SELECT run_id FROM runs WHERE session_id = ? AND lane = ? AND status = 'in_progress' \
             ORDER BY start_sequence DESC LIMIT 1";

/// History-database side of one projection pass.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct HistorySink<'a> {
    tx: Transaction<'static, Sqlite>,
    session_id: &'a SessionId,
    path: &'a str,
    cursor: Cursor,
    #[builder(default)]
    pub(super) source: Option<SourceRevision>,
    row_exists: bool,
    pub(super) path_changed: bool,
}

impl<'a> HistorySink<'a> {
    /// Reads this Session's cursor and recorded path inside the locked transaction.
    pub(crate) async fn open(
        mut tx: Transaction<'static, Sqlite>,
        session_id: &'a SessionId,
        path: &'a str,
    ) -> Result<Self, StateError> {
        let row: Option<Position> = sqlx::query_as(HISTORY_POSITION)
            .bind(session_id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        Ok(Self::builder()
            .tx(tx)
            .session_id(session_id)
            .path(path)
            .row_exists(row.is_some())
            .path_changed(row.as_ref().is_some_and(|row| row.path != path))
            .cursor(row.as_ref().map(|row| row.cursor).unwrap_or_default())
            .source(row.map(|row| row.source))
            .build())
    }

    /// Returns the position of the next line this database needs.
    pub(crate) fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Deletes the cursor row; runs, entries, and lanes cascade with it.
    pub(crate) async fn reset(&mut self) -> Result<(), StateError> {
        sqlx::query("DELETE FROM projection_state WHERE session_id = ?")
            .bind(self.session_id.as_str())
            .execute(&mut *self.tx)
            .await?;
        self.cursor = Cursor::default();
        self.source = None;
        self.row_exists = false;
        self.path_changed = false;
        Ok(())
    }

    /// Applies one line inside a savepoint and advances the cursor past it.
    pub(crate) async fn apply(
        &mut self,
        line: &LoggedMutation,
        timing: Option<MessageTiming>,
    ) -> Result<(), StateError> {
        // Dropping the savepoint on error rolls back only this line.
        let mut savepoint = (&mut *self.tx).begin().await?;
        HistoryLine::builder()
            .connection(&mut savepoint)
            .session_id(self.session_id)
            .path(self.path)
            .line(line)
            .build()
            .apply(timing)
            .await?;
        savepoint.commit().await?;
        if matches!(line.mutation, LogMutation::Header(_)) {
            self.row_exists = true;
            self.path_changed = false;
        }
        self.cursor.advance(line);
        Ok(())
    }

    /// Persists the cursor and canonical path, then commits.
    pub(crate) async fn commit(
        mut self,
        source: &SourceRevision,
    ) -> Result<(), StateError> {
        if self.row_exists {
            sqlx::query(
                "UPDATE projection_state SET path = ?, next_byte_offset = ?, next_sequence = ?, source_id = ?, source_size = ?, source_mtime = ? \
                 WHERE session_id = ?",
            )
            .bind(self.path)
            .bind(i64::try_from(self.cursor.next_byte_offset)?)
            .bind(i64::try_from(self.cursor.next_sequence)?)
            .bind(&source.source_id)
            .bind(i64::try_from(source.source_size)?)
            .bind(&source.source_mtime)
            .bind(self.session_id.as_str())
            .execute(&mut *self.tx)
            .await?;
        }
        self.tx.commit().await?;
        Ok(())
    }
}

/// History updates of one log line, run on its savepoint connection.
#[derive(typed_builder::TypedBuilder)]
struct HistoryLine<'c> {
    connection: &'c mut SqliteConnection,
    session_id: &'c SessionId,
    path: &'c str,
    line: &'c LoggedMutation,
}

impl HistoryLine<'_> {
    /// Routes the line to its history update.
    async fn apply(
        &mut self,
        timing: Option<MessageTiming>,
    ) -> Result<(), StateError> {
        let line = self.line;
        match &line.mutation {
            LogMutation::Header(_) => self.insert_cursor().await,
            LogMutation::Entry { lane, entry } => {
                self.insert_entry(lane, entry, timing).await
            }
            LogMutation::Lane { lane, leaf_id, .. } => {
                self.move_lane(lane, leaf_id.as_ref()).await
            }
            LogMutation::Record(record) => match record.kind {
                RecordKind::OperationStarted => self.start_run(record).await,
                RecordKind::OperationFinished => self.finish_run(record).await,
                _ => Ok(()),
            },
            LogMutation::Name { .. } | LogMutation::Label { .. } => Ok(()),
        }
    }

    /// Recreates the cursor row so rows of this Session can reference it.
    async fn insert_cursor(&mut self) -> Result<(), StateError> {
        // A header is applied only at offset zero, so any old rows are stale.
        sqlx::query("DELETE FROM projection_state WHERE session_id = ?")
            .bind(self.session_id.as_str())
            .execute(&mut *self.connection)
            .await?;
        sqlx::query(
            "INSERT INTO projection_state (session_id, path, next_byte_offset, next_sequence) \
             VALUES (?, ?, 0, 0)",
        )
        .bind(self.session_id.as_str())
        .bind(self.path)
        .execute(&mut *self.connection)
        .await?;
        Ok(())
    }

    /// Stores one entry, moves its lane's leaf, and links it to the lane's open Run.
    async fn insert_entry(
        &mut self,
        lane: &LaneId,
        entry: &SessionEntry,
        timing: Option<MessageTiming>,
    ) -> Result<(), StateError> {
        let run_id: Option<String> = sqlx::query_scalar(OPEN_RUN_SQL)
            .bind(self.session_id.as_str())
            .bind(lane.as_str())
            .fetch_optional(&mut *self.connection)
            .await?;
        let message_type = timing.and_then(|_| {
            entry
                .payload
                .get("content")
                .and_then(|content| content.get("type"))
                .and_then(serde_json::Value::as_str)
        });
        let started = timing
            .map(|timing| i64::try_from(timing.started_at_ms.get()))
            .transpose()?;
        let completed = timing
            .map(|timing| i64::try_from(timing.ended_at_ms.get()))
            .transpose()?;
        // An imported parent must already exist earlier in this Session's log.
        let inserted = sqlx::query(
            "INSERT INTO entries (session_id, entry_id, sequence, lane, parent_id, run_id, kind, \
             message_type, created_at_ms, started_at_ms, completed_at_ms, entry_json) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12 \
             WHERE ?5 IS NULL OR EXISTS (SELECT 1 FROM entries \
             WHERE session_id = ?1 AND entry_id = ?5 AND sequence < ?3)",
        )
        .bind(self.session_id.as_str())
        .bind(entry.id.as_str())
        .bind(i64::try_from(entry.sequence.get())?)
        .bind(lane.as_str())
        .bind(entry.parent_id.as_ref().map(EntryId::as_str))
        .bind(run_id.as_deref())
        .bind(entry.kind.as_str())
        .bind(message_type)
        .bind(i64::try_from(entry.timestamp_ms.get())?)
        .bind(started)
        .bind(completed)
        .bind(Json(entry))
        .execute(&mut *self.connection)
        .await?;
        if inserted.rows_affected() == 0 {
            return Err(StateError::InvalidParent {
                session_id: self.session_id.clone(),
                entry_id: entry.id.clone(),
            });
        }
        self.move_lane(lane, Some(&entry.id)).await?;
        let Some(run_id) = run_id else {
            return Ok(());
        };
        let link = match message_type {
            Some("user" | "expanded_user") => Some(
                "UPDATE runs SET first_user_entry_id = COALESCE(first_user_entry_id, ?) \
                 WHERE session_id = ? AND run_id = ?",
            ),
            Some("assistant") => Some(
                "UPDATE runs SET final_assistant_entry_id = ? WHERE session_id = ? AND run_id = ?",
            ),
            _ => None,
        };
        if let Some(sql) = link {
            sqlx::query(sql)
                .bind(entry.id.as_str())
                .bind(self.session_id.as_str())
                .bind(run_id)
                .execute(&mut *self.connection)
                .await?;
        }
        Ok(())
    }

    /// Points one lane at a new leaf, creating the lane on first sight.
    async fn move_lane(
        &mut self,
        lane: &LaneId,
        leaf: Option<&EntryId>,
    ) -> Result<(), StateError> {
        sqlx::query(
            "INSERT INTO lanes (session_id, lane, leaf_id) VALUES (?, ?, ?) \
             ON CONFLICT(session_id, lane) DO UPDATE SET leaf_id = excluded.leaf_id",
        )
        .bind(self.session_id.as_str())
        .bind(lane.as_str())
        .bind(leaf.map(EntryId::as_str))
        .execute(&mut *self.connection)
        .await?;
        Ok(())
    }

    /// Opens a Run at its `operation_started` record.
    async fn start_run(
        &mut self,
        record: &SessionRecord,
    ) -> Result<(), StateError> {
        let Some(run_id) = &record.run_id else {
            return Ok(());
        };
        // Agent Runs name their operation; compaction Runs carry a typed intent.
        let operation = record
            .payload
            .get("operation")
            .or_else(|| record.payload.get("intent")?.get("kind"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        // A repeated start of the same Run keeps the first one.
        sqlx::query(
            "INSERT INTO runs (session_id, run_id, lane, operation, status, start_sequence, \
             start_byte_offset, started_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(self.session_id.as_str())
        .bind(run_id.as_str())
        .bind(record.lane.as_str())
        .bind(operation)
        .bind(IN_PROGRESS)
        .bind(i64::try_from(record.sequence.get())?)
        .bind(i64::try_from(self.line.offset)?)
        .bind(i64::try_from(record.timestamp_ms.get())?)
        .execute(&mut *self.connection)
        .await?;
        Ok(())
    }

    /// Closes a Run at its `operation_finished` record with the verbatim status or outcome.
    async fn finish_run(
        &mut self,
        record: &SessionRecord,
    ) -> Result<(), StateError> {
        let Some(run_id) = &record.run_id else {
            return Ok(());
        };
        // Agent Runs record a `status`; compaction Runs record an `outcome`.
        let status = record
            .payload
            .get("status")
            .or_else(|| record.payload.get("outcome"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let error = record
            .payload
            .get("error")
            .map(serde_json::Value::to_string);
        let completed = i64::try_from(record.timestamp_ms.get())?;
        // MAX keeps a clock step backwards from producing a negative duration.
        sqlx::query(
            "UPDATE runs SET status = ?, error_json = ?, end_sequence = ?, end_byte_offset = ?, \
             completed_at_ms = ?, duration_ms = MAX(0, ? - started_at_ms) \
             WHERE session_id = ? AND run_id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(i64::try_from(record.sequence.get())?)
        .bind(i64::try_from(self.line.next_offset)?)
        .bind(completed)
        .bind(completed)
        .bind(self.session_id.as_str())
        .bind(run_id.as_str())
        .execute(&mut *self.connection)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::OPEN_RUN_SQL;
    use sqlx::Row;

    /// The per-entry run lookup never walks the Session's completed runs.
    #[tokio::test]
    async fn open_run_uses_lane_index() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("memory database");
        sqlx::migrate!("./migrations/history")
            .run(&pool)
            .await
            .expect("schema");
        let plan = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN {OPEN_RUN_SQL}"
        )))
        .bind("session")
        .bind("lane")
        .fetch_all(&pool)
        .await
        .expect("plan");
        let details: Vec<String> =
            plan.iter().map(|row| row.get("detail")).collect();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("SEARCH runs USING")
                    && detail.contains("idx_runs_open_lane")),
            "{details:?}"
        );
        assert!(
            !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
            "{details:?}"
        );
    }
}
