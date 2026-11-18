//! Applies log lines to the `sessions` row of one Session (spec §3.1, state rules).

use protocol::{
    AgentMessage, ContentBlock, MessageContent, ProductIdentity, SessionId,
    TimestampMs,
};
use sqlx::{Acquire, Sqlite, SqliteConnection, Transaction};
use store::{EntryKind, LogHeader, LogMutation, LoggedMutation, SessionEntry};

use super::{Cursor, Position, STATE_POSITION, SourceRevision};
use crate::StateError;
use crate::error::SqlText;

/// Characters of the first user message kept as the listing preview.
const PREVIEW_CHARS: usize = 200;

/// Metadata-database side of one projection pass.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct StateSink<'a> {
    tx: Transaction<'static, Sqlite>,
    session_id: &'a SessionId,
    path: &'a str,
    cursor: Cursor,
    #[builder(default)]
    pub(super) source: Option<SourceRevision>,
    #[builder(default)]
    last_timestamp: Option<i64>,
    row_exists: bool,
    pub(super) path_changed: bool,
}

impl<'a> StateSink<'a> {
    /// Reads this Session's cursor inside the already-locked transaction.
    pub(crate) async fn open(
        mut tx: Transaction<'static, Sqlite>,
        session_id: &'a SessionId,
        path: &'a str,
    ) -> Result<Self, StateError> {
        let row: Option<Position> = sqlx::query_as(STATE_POSITION)
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

    /// Deletes the Session row; the cursor row cascades with it.
    pub(crate) async fn reset(&mut self) -> Result<(), StateError> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(self.session_id.as_str())
            .execute(&mut *self.tx)
            .await?;
        self.cursor = Cursor::default();
        self.source = None;
        self.row_exists = false;
        self.last_timestamp = None;
        self.path_changed = false;
        Ok(())
    }

    /// Stages a line so the caller can discard it if the history write fails.
    pub(super) async fn stage(
        &mut self,
        line: &LoggedMutation,
        message: Option<&AgentMessage>,
    ) -> Result<Transaction<'_, Sqlite>, StateError> {
        // Dropping the savepoint on error rolls back only this line.
        let mut savepoint = (&mut *self.tx).begin().await?;
        StateLine {
            connection: &mut savepoint,
            session_id: self.session_id,
            path: self.path,
        }
        .apply(&line.mutation, message)
        .await?;
        Ok(savepoint)
    }

    /// Advances past a line that both databases accepted.
    pub(super) fn advance(
        &mut self,
        line: &LoggedMutation,
        timestamp: Option<i64>,
    ) {
        if matches!(line.mutation, LogMutation::Header(_)) {
            self.row_exists = true;
        }
        if timestamp.is_some() {
            self.last_timestamp = timestamp;
        }
        self.cursor.advance(line);
    }

    /// Persists the cursor next to the row it describes and commits.
    pub(crate) async fn commit(
        mut self,
        source: &SourceRevision,
    ) -> Result<(), StateError> {
        if let Some(timestamp) = self.last_timestamp {
            sqlx::query("UPDATE sessions SET updated_at_ms = ? WHERE id = ?")
                .bind(timestamp)
                .bind(self.session_id.as_str())
                .execute(&mut *self.tx)
                .await?;
        }
        if self.row_exists {
            sqlx::query(
                "INSERT INTO projection_state (session_id, next_byte_offset, next_sequence, source_id, source_size, source_mtime) \
                 VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(session_id) DO UPDATE SET \
                 next_byte_offset = excluded.next_byte_offset, next_sequence = excluded.next_sequence, \
                 source_id = excluded.source_id, source_size = excluded.source_size, source_mtime = excluded.source_mtime",
            )
            .bind(self.session_id.as_str())
            .bind(i64::try_from(self.cursor.next_byte_offset)?)
            .bind(i64::try_from(self.cursor.next_sequence)?)
            .bind(&source.source_id)
            .bind(i64::try_from(source.source_size)?)
            .bind(&source.source_mtime)
            .execute(&mut *self.tx)
            .await?;
        }
        self.tx.commit().await?;
        Ok(())
    }
}

/// Metadata updates of one log line, run on its savepoint connection.
struct StateLine<'c> {
    connection: &'c mut SqliteConnection,
    session_id: &'c SessionId,
    path: &'c str,
}

impl StateLine<'_> {
    /// Routes one decoded line to its metadata update.
    async fn apply(
        &mut self,
        mutation: &LogMutation,
        message: Option<&AgentMessage>,
    ) -> Result<(), StateError> {
        match mutation {
            LogMutation::Header(header) => self.insert_session(header).await,
            LogMutation::Entry { entry, .. } => match message {
                Some(message) => {
                    self.apply_message(message, entry.timestamp_ms).await
                }
                None => self.apply_entry(entry).await,
            },
            LogMutation::Record(_) => Ok(()),
            LogMutation::Name { name, .. } => {
                sqlx::query("UPDATE sessions SET name = ? WHERE id = ?")
                    .bind(name.as_deref())
                    .bind(self.session_id.as_str())
                    .execute(&mut *self.connection)
                    .await?;
                Ok(())
            }
            LogMutation::Lane { .. } | LogMutation::Label { .. } => Ok(()),
        }
    }

    /// Recreates the row from the header with derived columns at their initial values.
    async fn insert_session(
        &mut self,
        header: &LogHeader,
    ) -> Result<(), StateError> {
        let created = i64::try_from(header.created_at.get())?;
        let ownership = header.ownership.as_ref();
        let git = header.git.as_ref();
        // A header is applied only at offset zero, so any old row is stale.
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(self.session_id.as_str())
            .execute(&mut *self.connection)
            .await?;
        sqlx::query(
            "INSERT INTO sessions (id, path, cwd, created_at_ms, updated_at_ms, recency_at_ms, \
             parent_session_id, owner_session_id, agent_name, agent_nickname, \
             git_sha, git_branch, git_origin_url, app_version) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(self.session_id.as_str())
        .bind(self.path)
        .bind(header.cwd.sql_text()?)
        .bind(created)
        .bind(created)
        .bind(created)
        .bind(header.parent_session_id.as_ref().map(SessionId::as_str))
        .bind(ownership.map(|edge| edge.session_id.as_str()))
        .bind(ownership.map(|edge| AsRef::<str>::as_ref(&edge.name)))
        .bind(ownership.map(|edge| edge.nickname.as_str()))
        .bind(git.and_then(|git| git.sha.as_deref()))
        .bind(git.and_then(|git| git.branch.as_deref()))
        .bind(git.and_then(|git| git.origin_url.as_deref()))
        .bind(header.app_version.as_deref())
        .execute(&mut *self.connection)
        .await?;
        Ok(())
    }

    /// Applies the per-kind metadata rules of one transcript entry.
    async fn apply_entry(
        &mut self,
        entry: &SessionEntry,
    ) -> Result<(), StateError> {
        let field = |name: &str| {
            entry.payload.get(name).and_then(serde_json::Value::as_str)
        };
        match entry.kind {
            EntryKind::ModelChange => {
                sqlx::query(
                    "UPDATE sessions SET provider_id = COALESCE(?, provider_id), \
                     model_id = COALESCE(?, model_id) WHERE id = ?",
                )
                .bind(field("provider"))
                .bind(field("modelId"))
                .bind(self.session_id.as_str())
                .execute(&mut *self.connection)
                .await?;
                Ok(())
            }
            EntryKind::ThinkingLevelChange => {
                self.set_text(
                    "UPDATE sessions SET thinking_level = ? WHERE id = ?",
                    field("thinkingLevel"),
                )
                .await
            }
            // Only clawcode's own mode entry counts; an extension may reuse the name.
            EntryKind::Custom
                if field("extensionId") == Some(ProductIdentity::SLUG)
                    && field("customType") == Some("session_mode") =>
            {
                let mode = entry
                    .payload
                    .get("data")
                    .and_then(|data| data.get("mode"))
                    .and_then(serde_json::Value::as_str);
                self.set_text(
                    "UPDATE sessions SET session_mode = ? WHERE id = ?",
                    mode,
                )
                .await
            }
            _ => Ok(()),
        }
    }

    /// Applies user and assistant message rules through the protocol message type.
    async fn apply_message(
        &mut self,
        message: &AgentMessage,
        at: TimestampMs,
    ) -> Result<(), StateError> {
        let preview: Option<String> = match &message.content {
            MessageContent::User { blocks } => Some(
                blocks
                    .iter()
                    .filter_map(ContentBlock::text)
                    .enumerate()
                    .flat_map(|(i, text)| {
                        (i > 0).then_some('\n').into_iter().chain(text.chars())
                    })
                    .take(PREVIEW_CHARS)
                    .collect(),
            ),
            MessageContent::ExpandedUser { expansion } => Some(
                expansion
                    .invocation
                    .original
                    .chars()
                    .take(PREVIEW_CHARS)
                    .collect(),
            ),
            MessageContent::Assistant { metadata, .. } => {
                let tokens = metadata.usage.as_ref().total_tokens.unwrap_or(0);
                sqlx::query(
                    "UPDATE sessions SET provider_id = ?, model_id = ?, \
                     tokens_used = tokens_used + ? WHERE id = ?",
                )
                .bind(&metadata.provider_id)
                .bind(&metadata.model_id)
                .bind(i64::try_from(tokens)?)
                .bind(self.session_id.as_str())
                .execute(&mut *self.connection)
                .await?;
                None
            }
            _ => None,
        };
        if let Some(preview) = preview {
            sqlx::query(
                "UPDATE sessions SET has_user_message = 1, recency_at_ms = ?, \
                 preview = CASE WHEN preview = '' THEN ? ELSE preview END WHERE id = ?",
            )
            .bind(i64::try_from(at.get())?)
            .bind(preview)
            .bind(self.session_id.as_str())
            .execute(&mut *self.connection)
            .await?;
        }
        Ok(())
    }

    /// Runs a one-column `UPDATE`; a missing value leaves the column alone.
    async fn set_text(
        &mut self,
        sql: &'static str,
        value: Option<&str>,
    ) -> Result<(), StateError> {
        let Some(value) = value else {
            return Ok(());
        };
        sqlx::query(sql)
            .bind(value)
            .bind(self.session_id.as_str())
            .execute(&mut *self.connection)
            .await?;
        Ok(())
    }
}
