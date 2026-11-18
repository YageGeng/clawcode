//! SQLite session state and history mirrored from JSONL session logs.
//!
//! JSONL stays the source of truth; both databases hold only derived data and
//! can be deleted and rebuilt by the backfill, following Codex `codex-state`.

/// Codex requires SQLite 3.54 for the WAL-reset fix and RUSTSEC-2026-0459.
const MIN_SQLITE_VERSION_NUMBER: i32 = 3_054_000;
const _: () = assert!(
    libsqlite3_sys::SQLITE_VERSION_NUMBER >= MIN_SQLITE_VERSION_NUMBER,
    "bundled SQLite must include the WAL-reset and WAL page-size fixes",
);

mod backfill;
mod config;
mod database;
mod error;
mod history;
mod projector;
mod runtime;
mod sessions;
mod worker;

pub use backfill::{BackfillOutcome, StartupBackfill};
pub use config::{
    SESSION_HISTORY_DB_FILENAME, SESSION_HISTORY_DB_VERSION, STATE_DB_FILENAME,
    STATE_DB_VERSION, SqliteConfig,
};
pub use error::StateError;
pub use history::RunRow;
pub use runtime::StateRuntime;
pub use sessions::{
    SessionAnchor, SessionFilter, SessionRow, SessionScope, SessionsPage,
    SortKey,
};
pub use worker::StateWorker;
