//! Incremental projection of one JSONL Session log into both databases.
//!
//! Each database keeps its own cursor next to the rows it advances, so a
//! commit in one database can never be lost or double-applied by the other.
//! Both write transactions are opened with `BEGIN IMMEDIATE`, state first,
//! and the cursors are read inside them: a concurrent projector waits on the
//! lock and then sees the advanced cursors instead of re-applying lines.

mod history_sink;
mod state_sink;

use std::path::Path;

use protocol::SessionId;
use store::{LogMutation, LoggedMutation, MutationReader};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use self::history_sink::HistorySink;
use self::state_sink::StateSink;
use crate::error::SqlText;
use crate::{StateError, StateRuntime};

/// Position of the next unapplied line in one database (`projection_state` row).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct Cursor {
    pub(crate) next_byte_offset: u64,
    pub(crate) next_sequence: u64,
}

impl Cursor {
    /// Reports whether this cursor already moved past `line`.
    fn covers(&self, line: &LoggedMutation) -> bool {
        line.offset < self.next_byte_offset
    }

    /// Reports whether `line` is the one this cursor expects next.
    fn expects(&self, line: &LoggedMutation) -> bool {
        match line.mutation.sequence() {
            // The header is the only line at offset zero, before any sequence.
            None => *self == Self::default(),
            Some(sequence) => {
                self.next_sequence != 0 && sequence == self.next_sequence
            }
        }
    }

    /// Reports whether this cursor sits at the start of a line of `file`.
    ///
    /// Callers check the file is at least as long as the cursor first.
    async fn starts_line(
        &self,
        file: &mut tokio::fs::File,
    ) -> Result<bool, StateError> {
        let Some(before) = self.next_byte_offset.checked_sub(1) else {
            return Ok(true);
        };
        file.seek(std::io::SeekFrom::Start(before)).await?;
        Ok(file.read_u8().await? == b'\n')
    }

    /// Moves past `line`; the header is followed by sequence 1.
    fn advance(&mut self, line: &LoggedMutation) {
        self.next_byte_offset = line.next_offset;
        self.next_sequence =
            line.mutation.sequence().map_or(1, |sequence| sequence + 1);
    }
}

/// Result of applying the lines available in one read of the log.
enum Pass {
    /// Every complete line was applied.
    Done,
    /// A line arrived out of sequence for a sink; the log was rewritten.
    Mismatch(u64),
    /// A line could not be decoded or applied.
    Failed(StateError),
}

/// Brings one Session's rows in both databases up to its log.
pub(crate) struct SessionProjector<'a> {
    pub(crate) runtime: &'a StateRuntime,
    pub(crate) session_id: &'a SessionId,
    pub(crate) path: &'a Path,
}

impl SessionProjector<'_> {
    /// Applies every complete unapplied line and commits both cursors.
    pub(crate) async fn run(self) -> Result<(), StateError> {
        let path = self.path.sql_text()?;
        let state_tx = self.runtime.state.begin_with("BEGIN IMMEDIATE").await?;
        let history_tx =
            self.runtime.history.begin_with("BEGIN IMMEDIATE").await?;
        let mut state =
            StateSink::open(state_tx, self.session_id, path).await?;
        let mut history =
            HistorySink::open(history_tx, self.session_id, path).await?;
        let mut file = tokio::fs::File::open(self.path).await?;
        let length = file.metadata().await?.len();
        // A cursor past the end or inside a line means the log was rewritten.
        if history.path_changed()
            || length < state.cursor().next_byte_offset
            || length < history.cursor().next_byte_offset
            || !state.cursor().starts_line(&mut file).await?
            || !history.cursor().starts_line(&mut file).await?
        {
            self.reset(
                &mut state,
                &mut history,
                "log path changed or was rewritten",
            )
            .await?;
        }
        let mut restarted = false;
        let failure = loop {
            let start = state
                .cursor()
                .next_byte_offset
                .min(history.cursor().next_byte_offset);
            let mut bytes = Vec::new();
            file.seek(std::io::SeekFrom::Start(start)).await?;
            file.read_to_end(&mut bytes).await?;
            match self.apply(&bytes, start, &mut state, &mut history).await {
                Pass::Done => break None,
                Pass::Failed(error) => break Some(error),
                Pass::Mismatch(_) if !restarted => {
                    restarted = true;
                    self.reset(
                        &mut state,
                        &mut history,
                        "sequence does not continue the cursor",
                    )
                    .await?;
                }
                Pass::Mismatch(offset) => {
                    break Some(self.failure(
                        offset,
                        "sequence mismatch after reprojection",
                    ));
                }
            }
        };
        state.commit().await?;
        history.commit().await?;
        match failure {
            None => Ok(()),
            Some(error) => {
                tracing::warn!(
                    "stopped projecting session {} from {}: {}",
                    self.session_id,
                    self.path.display(),
                    error
                );
                Err(error)
            }
        }
    }

    /// Applies each complete line to every sink whose cursor has not passed it.
    async fn apply(
        &self,
        bytes: &[u8],
        start: u64,
        state: &mut StateSink<'_>,
        history: &mut HistorySink<'_>,
    ) -> Pass {
        let mut position = start;
        for item in MutationReader::new(bytes, start) {
            let line = match item {
                Ok(line) => line,
                Err(error) => {
                    return Pass::Failed(self.failure(position, error));
                }
            };
            // A file that names another Session can never be projected here.
            if let LogMutation::Header(header) = &line.mutation
                && &header.id != self.session_id
            {
                return Pass::Failed(self.failure(
                    line.offset,
                    format!("log header belongs to session {}", header.id),
                ));
            }
            // State first, history second, matching the transaction order.
            let cursor = state.cursor();
            if !cursor.covers(&line) {
                if !cursor.expects(&line) {
                    return Pass::Mismatch(line.offset);
                }
                if let Err(error) = state.apply(&line).await {
                    return Pass::Failed(self.failure(line.offset, error));
                }
            }
            let cursor = history.cursor();
            if !cursor.covers(&line) {
                if !cursor.expects(&line) {
                    return Pass::Mismatch(line.offset);
                }
                if let Err(error) = history.apply(&line).await {
                    return Pass::Failed(self.failure(line.offset, error));
                }
            }
            position = line.next_offset;
        }
        Pass::Done
    }

    /// Clears both databases for this Session so the next read starts at offset 0.
    async fn reset(
        &self,
        state: &mut StateSink<'_>,
        history: &mut HistorySink<'_>,
        reason: &str,
    ) -> Result<(), StateError> {
        tracing::warn!(
            "reprojecting session {} from {}: {}",
            self.session_id,
            self.path.display(),
            reason
        );
        state.reset().await?;
        history.reset().await
    }

    /// Builds the projection error reported for the line at `offset`.
    fn failure(
        &self,
        offset: u64,
        reason: impl std::fmt::Display,
    ) -> StateError {
        StateError::Projection {
            session_id: self.session_id.clone(),
            offset,
            reason: reason.to_string(),
        }
    }
}

impl StateRuntime {
    /// Projects every complete, not yet applied line of one Session log.
    pub async fn refresh(
        &self,
        session_id: &SessionId,
        path: &Path,
    ) -> Result<(), StateError> {
        SessionProjector {
            runtime: self,
            session_id,
            path,
        }
        .run()
        .await
    }
}
