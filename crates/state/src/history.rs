//! Session history reads over the history database (Codex `thread_turns` / `thread_items`).
//!
//! Every read first catches the Session up with its log, so a GUI reading
//! while no server runs still sees the latest entries.

use protocol::{
    EntryId, LaneId, RunId, SessionId, SessionTreeEntry, SessionTreeSnapshot,
    TimestampMs,
};
use store::SessionEntry;

use crate::{SessionRow, StateError, StateRuntime};

/// User-authored messages, served by `idx_entries_user_messages`.
const USER_ENTRIES_SQL: &str = "SELECT entry_json FROM entries WHERE session_id = ? \
     AND message_type IN ('user', 'expanded_user') ORDER BY sequence";

/// Ancestors of one entry walked through the primary key, root first.
const BRANCH_SQL: &str = "WITH RECURSIVE chain(entry_id, parent_id, entry_json, depth) AS ( \
     SELECT entry_id, parent_id, entry_json, 0 FROM entries WHERE session_id = ? AND entry_id = ? \
     UNION ALL \
     SELECT entries.entry_id, entries.parent_id, entries.entry_json, chain.depth + 1 \
     FROM entries JOIN chain ON entries.session_id = ? AND entries.entry_id = chain.parent_id) \
     SELECT entry_json FROM chain ORDER BY depth DESC";

/// One Run row with typed identifiers.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct RunRow {
    /// Run identifier.
    pub run_id: RunId,
    /// Lane the Run executed on.
    pub lane: LaneId,
    /// Operation kind from `operation_started` (agent / compaction / navigation).
    pub operation: String,
    /// `in_progress`, or the verbatim `operation_finished` status.
    pub status: String,
    /// JSON text of the finish error, when one was recorded.
    #[builder(default)]
    pub error_json: Option<String>,
    /// Log sequence of `operation_started`.
    pub start_sequence: u64,
    /// Log sequence of `operation_finished`.
    #[builder(default)]
    pub end_sequence: Option<u64>,
    /// Start time.
    pub started_at_ms: TimestampMs,
    /// Finish time.
    #[builder(default)]
    pub completed_at_ms: Option<TimestampMs>,
    /// Finish minus start.
    #[builder(default)]
    pub duration_ms: Option<u64>,
    /// First user message inside the Run.
    #[builder(default)]
    pub first_user_entry_id: Option<EntryId>,
    /// Last assistant message inside the Run.
    #[builder(default)]
    pub final_assistant_entry_id: Option<EntryId>,
}

/// Raw `runs` columns as SQLite returns them.
#[derive(sqlx::FromRow)]
struct RunColumns {
    run_id: String,
    lane: String,
    operation: String,
    status: String,
    error_json: Option<String>,
    start_sequence: u64,
    end_sequence: Option<u64>,
    started_at_ms: u64,
    completed_at_ms: Option<u64>,
    duration_ms: Option<u64>,
    first_user_entry_id: Option<String>,
    final_assistant_entry_id: Option<String>,
}

impl TryFrom<RunColumns> for RunRow {
    type Error = StateError;

    /// Validates identifiers and wraps timestamps.
    fn try_from(columns: RunColumns) -> Result<Self, Self::Error> {
        Ok(Self::builder()
            .run_id(RunId::try_from(columns.run_id)?)
            .lane(LaneId::try_from(columns.lane)?)
            .operation(columns.operation)
            .status(columns.status)
            .error_json(columns.error_json)
            .start_sequence(columns.start_sequence)
            .end_sequence(columns.end_sequence)
            .started_at_ms(columns.started_at_ms.into())
            .completed_at_ms(columns.completed_at_ms.map(TimestampMs::from))
            .duration_ms(columns.duration_ms)
            .first_user_entry_id(
                columns
                    .first_user_entry_id
                    .map(EntryId::try_from)
                    .transpose()?,
            )
            .final_assistant_entry_id(
                columns
                    .final_assistant_entry_id
                    .map(EntryId::try_from)
                    .transpose()?,
            )
            .build())
    }
}

/// The lossless `entry_json` column, decoded by sqlx.
#[derive(sqlx::FromRow)]
struct EntryJson {
    #[sqlx(json)]
    entry_json: SessionEntry,
}

impl From<EntryJson> for SessionTreeEntry {
    /// Projects a stored entry the same way the kernel's tree snapshot does.
    fn from(row: EntryJson) -> Self {
        let entry = row.entry_json;
        Self::builder()
            .entry_id(entry.id)
            .parent_id(entry.parent_id)
            .kind(entry.kind.as_str().to_string())
            .timestamp_ms(entry.timestamp_ms)
            .payload(serde_json::Value::Object(entry.payload))
            .build()
    }
}

impl StateRuntime {
    /// Catches a known Session up with its log; unknown Sessions return `None`.
    ///
    /// A failed catch-up is logged and the read continues with the rows already
    /// projected, so a damaged log never hides the history before the damage.
    async fn caught_up(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionRow>, StateError> {
        let Some(row) = self.session(id).await? else {
            return Ok(None);
        };
        if let Err(error) = self.refresh(id, &row.path).await {
            tracing::warn!(
                "serving projected history of session {} without catch-up: {}",
                id,
                error
            );
        }
        Ok(Some(row))
    }

    /// Rebuilds the kernel's tree snapshot of one lane without ACP.
    pub async fn tree_snapshot(
        &self,
        id: &SessionId,
        lane: &LaneId,
    ) -> Result<Option<SessionTreeSnapshot>, StateError> {
        let Some(row) = self.caught_up(id).await? else {
            return Ok(None);
        };
        let entries: Vec<EntryJson> = sqlx::query_as(
            "SELECT entry_json FROM entries WHERE session_id = ? ORDER BY sequence",
        )
        .bind(id.as_str())
        .fetch_all(&self.history)
        .await?;
        let leaf: Option<Option<String>> = sqlx::query_scalar(
            "SELECT leaf_id FROM lanes WHERE session_id = ? AND lane = ?",
        )
        .bind(id.as_str())
        .bind(lane.as_str())
        .fetch_optional(&self.history)
        .await?;
        // A lane that was never written starts at the root, like the store's `main`.
        Ok(Some(
            SessionTreeSnapshot::builder()
                .session_id(id.clone())
                .lane(lane.clone())
                .leaf_id(leaf.flatten().map(EntryId::try_from).transpose()?)
                .entries(
                    entries.into_iter().map(SessionTreeEntry::from).collect(),
                )
                .name(row.name)
                .build(),
        ))
    }

    /// Returns the branch ending at `leaf`, root first.
    pub async fn branch(
        &self,
        id: &SessionId,
        leaf: &EntryId,
    ) -> Result<Vec<SessionTreeEntry>, StateError> {
        if self.caught_up(id).await?.is_none() {
            return Ok(Vec::new());
        }
        let rows: Vec<EntryJson> = sqlx::query_as(BRANCH_SQL)
            .bind(id.as_str())
            .bind(leaf.as_str())
            .bind(id.as_str())
            .fetch_all(&self.history)
            .await?;
        Ok(rows.into_iter().map(SessionTreeEntry::from).collect())
    }

    /// Pages Runs in log order after `after_start_sequence`.
    pub async fn runs(
        &self,
        id: &SessionId,
        after_start_sequence: Option<u64>,
        limit: u32,
    ) -> Result<Vec<RunRow>, StateError> {
        if self.caught_up(id).await?.is_none() {
            return Ok(Vec::new());
        }
        let after = after_start_sequence.map(i64::try_from).transpose()?;
        sqlx::query_as::<_, RunColumns>(
            "SELECT * FROM runs WHERE session_id = ? AND start_sequence > ? \
             ORDER BY start_sequence LIMIT ?",
        )
        .bind(id.as_str())
        .bind(after.unwrap_or(-1))
        .bind(i64::from(limit))
        .fetch_all(&self.history)
        .await?
        .into_iter()
        .map(RunRow::try_from)
        .collect()
    }

    /// Returns the entries appended inside one Run, in log order.
    pub async fn run_entries(
        &self,
        id: &SessionId,
        run: &RunId,
    ) -> Result<Vec<SessionTreeEntry>, StateError> {
        if self.caught_up(id).await?.is_none() {
            return Ok(Vec::new());
        }
        let rows: Vec<EntryJson> = sqlx::query_as(
            "SELECT entry_json FROM entries WHERE session_id = ? AND run_id = ? ORDER BY sequence",
        )
        .bind(id.as_str())
        .bind(run.as_str())
        .fetch_all(&self.history)
        .await?;
        Ok(rows.into_iter().map(SessionTreeEntry::from).collect())
    }

    /// Returns the user-authored messages, for navigation between prompts.
    pub async fn user_entries(
        &self,
        id: &SessionId,
    ) -> Result<Vec<SessionTreeEntry>, StateError> {
        if self.caught_up(id).await?.is_none() {
            return Ok(Vec::new());
        }
        let rows: Vec<EntryJson> = sqlx::query_as(USER_ENTRIES_SQL)
            .bind(id.as_str())
            .fetch_all(&self.history)
            .await?;
        Ok(rows.into_iter().map(SessionTreeEntry::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Row;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    /// User-message navigation is served by its partial index.
    #[tokio::test]
    async fn user_entries_use_user_message_index() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("memory database");
        sqlx::migrate!("./session_history_migrations")
            .run(&pool)
            .await
            .expect("migrate");
        let plan: Vec<String> = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN {USER_ENTRIES_SQL}"
        )))
        .bind("session")
        .fetch_all(&pool)
        .await
        .expect("plan")
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect();
        assert!(
            plan.join(" ").contains("idx_entries_user_messages"),
            "{plan:?}"
        );
    }
}
