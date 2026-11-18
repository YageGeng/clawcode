//! Errors raised while opening, projecting, or querying the state databases.

use std::path::{Path, PathBuf};

use protocol::SessionId;

/// SQLite primary result code for a busy database.
const SQLITE_BUSY: i32 = 5;
/// SQLite primary result code for a malformed database image.
const SQLITE_CORRUPT: i32 = 11;
/// SQLite primary result code for a file that is not a database.
const SQLITE_NOTADB: i32 = 26;

/// Failure of a state database operation; JSONL remains authoritative.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// SQLite or connection-pool failure.
    #[error("state database failed: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Embedded migration failure.
    #[error("state database migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// Filesystem failure.
    #[error("state database I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSONL log could not be read.
    #[error("session log failed: {0}")]
    Store(#[from] store::StoreError),
    /// JSON payload could not be encoded or decoded.
    #[error("state JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// A stored identifier is not a valid protocol scalar.
    #[error("state column holds an invalid identifier: {0}")]
    Scalar(#[from] protocol::ScalarError),
    /// A stored value names an invalid agent.
    #[error("state column holds an invalid agent name: {0}")]
    AgentName(String),
    /// A count or timestamp does not fit SQLite's signed integers.
    #[error("state integer out of range: {0}")]
    IntegerRange(#[from] std::num::TryFromIntError),
    /// One Session log line could not be applied.
    #[error(
        "projection of session {session_id} stopped at byte {offset}: {reason}"
    )]
    Projection {
        /// Session being projected.
        session_id: SessionId,
        /// Absolute offset of the line that failed.
        offset: u64,
        /// Why the line could not be applied.
        reason: String,
    },
    /// A path cannot be stored as SQLite text.
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
}

impl StateError {
    /// Reports whether SQLite rejected the file as malformed or not a database.
    #[must_use]
    pub fn is_corruption(&self) -> bool {
        matches!(self.primary_code(), Some(SQLITE_CORRUPT | SQLITE_NOTADB))
    }

    /// Reports whether another connection held the lock past the busy timeout.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.primary_code() == Some(SQLITE_BUSY)
    }

    /// Extracts the SQLite primary result code from a wrapped database error.
    fn primary_code(&self) -> Option<i32> {
        let error = match self {
            Self::Sqlx(error)
            | Self::Migrate(
                sqlx::migrate::MigrateError::Execute(error)
                | sqlx::migrate::MigrateError::ExecuteMigration(error, _),
            ) => error,
            _ => return None,
        };
        let code = error.as_database_error()?.code()?;
        code.parse::<i32>().ok().map(|extended| extended & 0xff)
    }
}

/// Borrows a filesystem path as SQLite text.
pub(crate) trait SqlText {
    /// Returns the path as UTF-8, or [`StateError::NonUtf8Path`].
    fn sql_text(&self) -> Result<&str, StateError>;
}

impl SqlText for Path {
    /// Rejects paths SQLite text cannot represent losslessly.
    fn sql_text(&self) -> Result<&str, StateError> {
        self.to_str()
            .ok_or_else(|| StateError::NonUtf8Path(self.to_path_buf()))
    }
}
