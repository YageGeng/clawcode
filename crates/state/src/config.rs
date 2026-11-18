//! Locations of the state databases.

use std::path::{Path, PathBuf};

/// Derives both public constants from one literal so paths stay static and each
/// database can change file formats independently of its SQLx migrations.
macro_rules! database_file {
    ($stem:literal, $version:ident = $number:literal, $filename:ident) => {
        #[doc = concat!("On-disk file format version of the `", $stem, "` database.")]
        pub const $version: u32 = $number;
        #[doc = concat!("Filename for the current `", $stem, "` database.")]
        pub const $filename: &str = concat!($stem, "_", $number, ".sqlite");
    };
}

database_file!("state", STATE_DB_VERSION = 1, STATE_DB_FILENAME);
database_file!(
    "session_history",
    SESSION_HISTORY_DB_VERSION = 1,
    SESSION_HISTORY_DB_FILENAME
);

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
