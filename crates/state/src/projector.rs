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

use protocol::{AgentMessage, SessionId};
use serde::{Deserialize, de::value::MapDeserializer};
use store::{EntryKind, LogMutation, LoggedMutation, MutationReader};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};

use self::history_sink::HistorySink;
use self::state_sink::StateSink;
use crate::error::SqlText;
use crate::{StateError, StateRuntime};

/// Yield the writers after either limit, always finishing the current JSONL line.
const BATCH_LINES: usize = 128;
const BATCH_BYTES: usize = 256 * 1024;

const STATE_POSITION: &str = "SELECT s.path, p.next_byte_offset, p.next_sequence, p.source_id, p.source_size, p.source_mtime \
    FROM projection_state p JOIN sessions s ON s.id = p.session_id WHERE p.session_id = ?";
const HISTORY_POSITION: &str = "SELECT path, next_byte_offset, next_sequence, source_id, source_size, source_mtime \
    FROM projection_state WHERE session_id = ?";

/// Filesystem revision sampled from the open log before reading a batch.
#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct SourceRevision {
    pub(crate) source_id: String,
    pub(crate) source_size: u64,
    pub(crate) source_mtime: String,
}

impl TryFrom<&std::fs::Metadata> for SourceRevision {
    type Error = std::io::Error;

    /// Keeps file identity separate from the timestamp so ordinary appends stay incremental.
    fn try_from(metadata: &std::fs::Metadata) -> Result<Self, Self::Error> {
        #[cfg(unix)]
        let (source_id, source_mtime) = {
            use std::os::unix::fs::MetadataExt;
            // ctime also catches in-place edits that restore the original modification time.
            (
                format!("{}:{}", metadata.dev(), metadata.ino()),
                format!(
                    "{}:{}:{}:{}",
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec()
                ),
            )
        };
        #[cfg(not(unix))]
        let (source_id, source_mtime) = {
            #[cfg(windows)]
            let source_id = {
                use std::os::windows::fs::MetadataExt;
                metadata.creation_time().to_string()
            };
            #[cfg(not(windows))]
            let source_id = String::from("file");
            let modified =
                metadata.modified()?.duration_since(std::time::UNIX_EPOCH);
            let mtime = match modified {
                Ok(elapsed) => elapsed.as_nanos().to_string(),
                Err(before) => format!("-{}", before.duration().as_nanos()),
            };
            (source_id, mtime)
        };
        Ok(Self {
            source_id,
            source_size: metadata.len(),
            source_mtime,
        })
    }
}

impl SourceRevision {
    /// An older index, a replaced file, or a changed non-growing log requires a full replay.
    fn rewritten_by(&self, current: &Self) -> bool {
        self.source_id != current.source_id
            || current.source_size < self.source_size
            || (self.source_size == current.source_size
                && self.source_mtime != current.source_mtime)
    }
}

/// A cursor is meaningful only for the canonical path it was projected from.
#[derive(PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct Position {
    pub(crate) path: String,
    #[sqlx(flatten)]
    pub(crate) source: SourceRevision,
    #[sqlx(flatten)]
    pub(crate) cursor: Cursor,
}

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

/// One pair of locked sinks; their cursors and savepoints belong to this batch only.
struct ProjectionBatch<'a> {
    projector: &'a SessionProjector<'a>,
    state: StateSink<'a>,
    history: HistorySink<'a>,
}

impl SessionProjector<'_> {
    /// Checks both published cursors without taking either SQLite writer lock.
    async fn is_current(&self, path: &str) -> Result<bool, StateError> {
        let (state, history) = tokio::try_join!(
            sqlx::query_as::<_, Position>(STATE_POSITION)
                .bind(self.session_id.as_str())
                .fetch_optional(&self.runtime.state),
            sqlx::query_as::<_, Position>(HISTORY_POSITION)
                .bind(self.session_id.as_str())
                .fetch_optional(&self.runtime.history),
        )?;
        if let Some(state) = state
            && history.as_ref() == Some(&state)
            && state.path == path
        {
            let mut file = tokio::fs::File::open(self.path).await?;
            let metadata = file.metadata().await?;
            if metadata.len() == state.cursor.next_byte_offset
                && state.source == SourceRevision::try_from(&metadata)?
                && state.cursor.starts_line(&mut file).await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Applies complete lines up to the initial file length, releasing the writers between batches.
    pub(crate) async fn run(self) -> Result<(), StateError> {
        let path = self.path.sql_text()?;
        if self.is_current(path).await? {
            return Ok(());
        }
        let mut end = None;
        let mut restarted = false;
        let mut bytes = Vec::new();
        loop {
            // Reopen the file and reread both cursors under the locks after each commit.
            // Another projector or a deletion may have finished while we were waiting.
            let mut batch = self.begin_batch(path).await?;
            let mut file = tokio::fs::File::open(self.path).await?;
            let source = SourceRevision::try_from(&file.metadata().await?)?;
            let length = source.source_size;
            // Appends that arrive during catch-up belong to the next refresh.
            let end = *end.get_or_insert(length);
            let mut start = batch.start(&mut file, &source).await?;
            let result = loop {
                let remaining = length.min(end).saturating_sub(start);
                file.seek(std::io::SeekFrom::Start(start)).await?;
                let mut reader = BufReader::new((&mut file).take(remaining));
                bytes.clear();
                for _ in 0..BATCH_LINES {
                    if reader.read_until(b'\n', &mut bytes).await? == 0
                        || bytes.len() >= BATCH_BYTES
                    {
                        break;
                    }
                }
                match batch.apply(&bytes, start).await {
                    Pass::Done => {
                        break Ok(u64::try_from(bytes.len())? >= remaining
                            || !bytes.ends_with(b"\n"));
                    }
                    Pass::Failed(error) => break Err(error),
                    Pass::Mismatch(_) if !restarted => {
                        // Republish the header in this transaction, so the identity stays visible.
                        batch
                            .reset("sequence does not continue the cursor")
                            .await?;
                        restarted = true;
                        start = 0;
                    }
                    Pass::Mismatch(offset) => {
                        break Err(self.failure(
                            offset,
                            "sequence mismatch after reprojection",
                        ));
                    }
                }
            };
            batch.state.commit(&source).await?;
            batch.history.commit(&source).await?;
            match result {
                Ok(true) => return Ok(()),
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(
                        "stopped projecting session {} from {}: {}",
                        self.session_id,
                        self.path.display(),
                        error
                    );
                    return Err(error);
                }
            }
        }
    }

    /// Locks the databases in their shared write order before fetching either cursor.
    async fn begin_batch<'a>(
        &'a self,
        path: &'a str,
    ) -> Result<ProjectionBatch<'a>, StateError> {
        let state = self.runtime.state.begin_with("BEGIN IMMEDIATE").await?;
        let history =
            self.runtime.history.begin_with("BEGIN IMMEDIATE").await?;
        Ok(ProjectionBatch {
            projector: self,
            state: StateSink::open(state, self.session_id, path).await?,
            history: HistorySink::open(history, self.session_id, path).await?,
        })
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

impl ProjectionBatch<'_> {
    /// Locates the earliest pending line, resetting stale cursors before either sink advances.
    async fn start(
        &mut self,
        file: &mut tokio::fs::File,
        source: &SourceRevision,
    ) -> Result<u64, StateError> {
        let length = source.source_size;
        let state = self.state.cursor();
        let history = self.history.cursor();
        let same_offset = state.next_byte_offset == history.next_byte_offset;
        if self.state.path_changed
            || self.history.path_changed
            || self
                .state
                .source
                .as_ref()
                .is_some_and(|old| old.rewritten_by(source))
            || self
                .history
                .source
                .as_ref()
                .is_some_and(|old| old.rewritten_by(source))
            || (same_offset && state.next_sequence != history.next_sequence)
            || length < state.next_byte_offset.max(history.next_byte_offset)
            || !state.starts_line(file).await?
            || (!same_offset && !history.starts_line(file).await?)
        {
            self.reset("log path changed or was rewritten").await?;
            return Ok(0);
        }
        Ok(state.next_byte_offset.min(history.next_byte_offset))
    }

    /// Applies each complete line to every sink whose cursor has not passed it.
    async fn apply(&mut self, bytes: &[u8], start: u64) -> Pass {
        let mut position = start;
        for item in MutationReader::new(bytes, start) {
            let line = match item {
                Ok(line) => line,
                Err(error) => {
                    return Pass::Failed(
                        self.projector.failure(position, error),
                    );
                }
            };
            // A file that names another Session can never be projected here.
            if let LogMutation::Header(header) = &line.mutation
                && &header.id != self.projector.session_id
            {
                return Pass::Failed(self.projector.failure(
                    line.offset,
                    format!("log header belongs to session {}", header.id),
                ));
            }
            if [self.state.cursor(), self.history.cursor()]
                .iter()
                .any(|cursor| !cursor.covers(&line) && !cursor.expects(&line))
            {
                return Pass::Mismatch(line.offset);
            }
            if let Err(error) = self.apply_line(&line).await {
                if error.is_busy() {
                    return Pass::Failed(error);
                }
                return Pass::Failed(
                    self.projector.failure(line.offset, error),
                );
            }
            position = line.next_offset;
        }
        Pass::Done
    }

    /// Validates a message once and advances only the sinks that still need it.
    async fn apply_line(
        &mut self,
        line: &LoggedMutation,
    ) -> Result<(), StateError> {
        let message = match &line.mutation {
            LogMutation::Entry { entry, .. }
                if entry.kind == EntryKind::Message =>
            {
                let fields = entry
                    .payload
                    .iter()
                    .map(|(key, value)| (key.as_str(), value));
                Some(AgentMessage::deserialize(MapDeserializer::new(fields))?)
            }
            _ => None,
        };
        let update_state = !self.state.cursor().covers(line);
        // Reject out-of-range timestamps at their line, before either cursor advances.
        let timestamp = match &line.mutation {
            LogMutation::Entry { entry, .. } => Some(entry.timestamp_ms),
            LogMutation::Record(record) => Some(record.timestamp_ms),
            _ => None,
        }
        .map(|at| i64::try_from(at.get()))
        .transpose()?;
        // Keep the metadata savepoint open until history accepts the same line.
        {
            // Records and lane/label facts advance metadata progress without touching the row.
            let staged = if update_state
                && !matches!(
                    line.mutation,
                    LogMutation::Record(_)
                        | LogMutation::Lane { .. }
                        | LogMutation::Label { .. }
                ) {
                Some(self.state.stage(line, message.as_ref()).await?)
            } else {
                None
            };
            if !self.history.cursor().covers(line) {
                self.history
                    .apply(line, message.as_ref().map(|message| message.timing))
                    .await?;
            }
            if let Some(staged) = staged {
                staged.commit().await?;
            }
        }
        if update_state {
            self.state.advance(line, timestamp);
        }
        Ok(())
    }

    /// Clears both databases for this Session so the next read starts at offset 0.
    async fn reset(&mut self, reason: &str) -> Result<(), StateError> {
        tracing::warn!(
            "reprojecting session {} from {}: {}",
            self.projector.session_id,
            self.projector.path.display(),
            reason
        );
        self.state.reset().await?;
        self.history.reset().await
    }
}

impl StateRuntime {
    /// Projects every complete, not yet applied line of one Session log.
    pub async fn refresh(
        &self,
        session_id: &SessionId,
        path: &Path,
    ) -> Result<(), StateError> {
        let path = std::path::absolute(path)?;
        SessionProjector {
            runtime: self,
            session_id,
            path: &path,
        }
        .run()
        .await
    }
}
