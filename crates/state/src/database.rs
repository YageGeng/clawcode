//! Opens one state database with Codex's connection settings and migrations.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePool,
    SqlitePoolOptions, SqliteSynchronous,
};

use store::{Clock, SystemClock};

use crate::StateError;

/// Lock wait shared by every connection (Codex `DEFAULT_BUSY_TIMEOUT`).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Connections per database pool.
const MAX_CONNECTIONS: u32 = 5;
/// Startup retry delays after `SQLITE_BUSY` or a lost migration race (Codex `busy_retry.rs`).
const BUSY_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(100), Duration::from_millis(200)];

/// Opens one database file and brings its schema up to date.
pub(crate) struct DatabaseOpener<'a> {
    pub(crate) path: &'a Path,
    pub(crate) migrator: Migrator,
}

impl DatabaseOpener<'_> {
    /// Opens the pool; a corrupt file is set aside once and recreated.
    pub(crate) async fn open(self) -> Result<SqlitePool, StateError> {
        match self.open_with_retry().await {
            Err(error) if error.is_corruption() => {
                self.back_up()?;
                self.open_with_retry().await
            }
            result => result,
        }
    }

    /// Retries startup while another process holds the lock or migrates first.
    ///
    /// Two processes migrating a fresh file race on the same `CREATE TABLE`;
    /// the loser's transaction rolls back and its retry finds the schema applied.
    async fn open_with_retry(&self) -> Result<SqlitePool, StateError> {
        let mut delays = BUSY_RETRY_DELAYS.into_iter();
        loop {
            match self.connect().await {
                Err(error)
                    if error.is_busy()
                        || matches!(error, StateError::Migrate(_)) =>
                {
                    let Some(delay) = delays.next() else {
                        return Err(error);
                    };
                    tracing::warn!(
                        "state database {} could not open ({}); retrying in {:?}",
                        self.path.display(),
                        error,
                        delay
                    );
                    tokio::time::sleep(delay).await;
                }
                result => return result,
            }
        }
    }

    /// Connects with WAL settings and applies the embedded migrations.
    async fn connect(&self) -> Result<SqlitePool, StateError> {
        let options = SqliteConnectOptions::new()
            .filename(self.path)
            .create_if_missing(true)
            .auto_vacuum(SqliteAutoVacuum::Incremental)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .connect_with(options)
            .await?;
        if let Err(error) = self.migrator.run(&pool).await {
            pool.close().await;
            return Err(error.into());
        }
        tracing::info!("opened state database {}", self.path.display());
        Ok(pool)
    }

    /// Renames the database and its WAL sidecars to `<name>.corrupt-<unix ms>`.
    fn back_up(&self) -> Result<(), StateError> {
        let stamp = SystemClock.now();
        for suffix in ["", "-wal", "-shm"] {
            let mut source = self.path.as_os_str().to_owned();
            source.push(suffix);
            let source = PathBuf::from(source);
            let mut target = source.as_os_str().to_owned();
            target.push(format!(".corrupt-{stamp}"));
            match std::fs::rename(&source, &target) {
                Ok(()) => tracing::warn!(
                    "moved corrupt state database {} to {}",
                    source.display(),
                    PathBuf::from(target).display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
