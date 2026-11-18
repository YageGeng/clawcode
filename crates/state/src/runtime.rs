//! Process-wide handle to both state databases.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::SqlitePool;

use crate::database::DatabaseOpener;
use crate::{SqliteConfig, StateError};

/// Session metadata and history pools shared by the kernel and external readers.
pub struct StateRuntime {
    pub(crate) state: SqlitePool,
    pub(crate) history: SqlitePool,
    pub(crate) backfill_complete: AtomicBool,
}

impl StateRuntime {
    /// Opens and migrates both databases under the configured home.
    pub async fn init(config: &SqliteConfig) -> Result<Arc<Self>, StateError> {
        tokio::fs::create_dir_all(config.home()).await?;
        let mut state_migrator = sqlx::migrate!("./migrations/state");
        // Older binaries may open a database a newer binary migrated (Codex rule).
        state_migrator.set_ignore_missing(true);
        let mut history_migrator = sqlx::migrate!("./migrations/history");
        history_migrator.set_ignore_missing(true);
        let state_path = config.state_db_path();
        let state = DatabaseOpener {
            path: &state_path,
            migrator: state_migrator,
        }
        .open()
        .await?;
        let history_path = config.session_history_db_path();
        let history = match (DatabaseOpener {
            path: &history_path,
            migrator: history_migrator,
        })
        .open()
        .await
        {
            Ok(history) => history,
            Err(error) => {
                state.close().await;
                return Err(error);
            }
        };
        Ok(Arc::new(Self {
            state,
            history,
            // A previous run's watermark cannot certify either database after an offline change.
            backfill_complete: AtomicBool::new(false),
        }))
    }

    /// Reports whether the backfill finished, so SQLite answers are complete.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.backfill_complete.load(Ordering::Acquire)
    }
}
