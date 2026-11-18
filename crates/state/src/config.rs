//! Locations of the state databases.

use std::path::{Path, PathBuf};

/// Session metadata and backfill state (Codex `state_<n>.sqlite`).
pub const STATE_DB_FILENAME: &str = "state_1.sqlite";

/// Paginated Session history (Codex `thread_history_<n>.sqlite`).
pub const SESSION_HISTORY_DB_FILENAME: &str = "session_history_1.sqlite";

/// Directory that holds every state database file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteConfig {
    home: PathBuf,
}

impl SqliteConfig {
    /// Places the databases directly inside `home`.
    #[must_use]
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// Returns the directory that holds the database files.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Returns the Session metadata database path.
    #[must_use]
    pub fn state_db_path(&self) -> PathBuf {
        self.home.join(STATE_DB_FILENAME)
    }

    /// Returns the Session history database path.
    #[must_use]
    pub fn session_history_db_path(&self) -> PathBuf {
        self.home.join(SESSION_HISTORY_DB_FILENAME)
    }
}
