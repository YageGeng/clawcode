use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use state::{
    SESSION_HISTORY_DB_VERSION, STATE_DB_FILENAME, STATE_DB_VERSION,
    SqliteConfig, StateRuntime,
};

/// Opens an independent pool on one database file, as an external reader would.
async fn reader(path: &Path) -> SqlitePool {
    SqlitePool::connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .expect("external pool")
}

/// Lists schema object names of one kind, sorted.
async fn names(pool: &SqlitePool, kind: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = ? AND name NOT LIKE 'sqlite_%' \
         AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .bind(kind)
    .fetch_all(pool)
    .await
    .expect("schema names")
}

/// The bundled SQLite carries the WAL fixes Codex requires.
#[tokio::test]
async fn bundled_sqlite_is_at_least_3_54() {
    let home = tempfile::tempdir().expect("home");
    let config = SqliteConfig::new(home.path());
    StateRuntime::init(&config).await.expect("init");
    let pool = reader(&config.state_db_path()).await;
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await
        .expect("version");
    let parts = version
        .split('.')
        .map(|part| part.parse::<u32>().expect("numeric"))
        .collect::<Vec<_>>();
    assert!(
        parts[..2] >= [3, 54][..],
        "SQLite {version} is older than 3.54"
    );
}

/// Both databases exist with the exact table and index set, in WAL mode.
#[tokio::test]
async fn init_creates_both_databases() {
    let home = tempfile::tempdir().expect("home");
    let config = SqliteConfig::new(home.path().join("nested"));
    StateRuntime::init(&config).await.expect("init");
    assert_eq!(
        config.state_db_path(),
        config
            .home()
            .join(format!("state_{STATE_DB_VERSION}.sqlite"))
    );
    assert_eq!(
        config.session_history_db_path(),
        config.home().join(format!(
            "session_history_{SESSION_HISTORY_DB_VERSION}.sqlite"
        ))
    );

    let state = reader(&config.state_db_path()).await;
    assert_eq!(
        names(&state, "table").await,
        ["backfill_state", "projection_state", "sessions"]
    );
    let state_indexes = names(&state, "index").await;
    assert_eq!(state_indexes.len(), 14);
    assert!(
        state_indexes
            .iter()
            .all(|name| name.starts_with("idx_sessions_"))
    );
    let history = reader(&config.session_history_db_path()).await;
    for pool in [&state, &history] {
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(pool)
            .await
            .expect("journal mode");
        let vacuum: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
            .fetch_one(pool)
            .await
            .expect("auto vacuum mode");
        assert_eq!((journal.as_str(), vacuum), ("wal", 2));
    }
    assert_eq!(
        names(&history, "table").await,
        [
            "entries",
            "lanes",
            "projection_state",
            "records",
            "request_contents",
            "runs"
        ]
    );
    assert_eq!(
        names(&history, "index").await,
        [
            "idx_entries_by_run_page",
            "idx_entries_page",
            "idx_entries_user_messages",
            "idx_records_by_kind",
            "idx_runs_end_page",
            "idx_runs_open_lane",
            "idx_runs_page",
        ]
    );
}

/// A fresh database has one pending backfill row and is not ready.
#[tokio::test]
async fn init_starts_with_pending_backfill() {
    let home = tempfile::tempdir().expect("home");
    let config = SqliteConfig::new(home.path());
    let runtime = StateRuntime::init(&config).await.expect("init");
    assert!(!runtime.is_ready());
    let state = reader(&config.state_db_path()).await;
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM backfill_state")
            .fetch_all(&state)
            .await
            .expect("backfill rows");
    assert_eq!(statuses, ["pending"]);
}

/// Two processes opening a fresh home at once both get a working runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_fresh_init_both_succeed() {
    for _ in 0..10 {
        let home = tempfile::tempdir().expect("home");
        let config = SqliteConfig::new(home.path());
        let (first, second) = tokio::join!(
            StateRuntime::init(&config),
            StateRuntime::init(&config)
        );
        first.expect("first init");
        second.expect("second init");
    }
}

/// An older binary still opens a database a newer binary migrated further.
#[tokio::test]
async fn init_opens_database_migrated_by_newer_binary() {
    let home = tempfile::tempdir().expect("home");
    let config = SqliteConfig::new(home.path());
    drop(StateRuntime::init(&config).await.expect("first init"));
    let state = reader(&config.state_db_path()).await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) \
         SELECT 9999, 'future', installed_on, success, checksum, execution_time \
         FROM _sqlx_migrations WHERE version = 1",
    )
    .execute(&state)
    .await
    .expect("future migration row");
    state.close().await;

    StateRuntime::init(&config).await.expect("second init");
}

/// A file that is not a database is set aside and replaced by a fresh one.
#[tokio::test]
async fn init_backs_up_corrupt_database() {
    let home = tempfile::tempdir().expect("home");
    let config = SqliteConfig::new(home.path());
    std::fs::write(config.state_db_path(), b"not a database").expect("garbage");

    StateRuntime::init(&config).await.expect("init");

    let backup_prefix = format!("{STATE_DB_FILENAME}.corrupt-");
    let backups = std::fs::read_dir(home.path())
        .expect("home listing")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&backup_prefix)
        })
        .count();
    assert_eq!(backups, 1);
    let state = reader(&config.state_db_path()).await;
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&state)
        .await
        .expect("sessions");
    assert_eq!(rows, 0);
}

/// Ownership columns are all-or-nothing.
#[tokio::test]
async fn sessions_reject_partial_ownership() {
    let home = tempfile::tempdir().expect("home");
    let config = SqliteConfig::new(home.path());
    StateRuntime::init(&config).await.expect("init");
    let state = reader(&config.state_db_path()).await;
    let result = sqlx::query(
        "INSERT INTO sessions (id, path, cwd, created_at_ms, updated_at_ms, recency_at_ms, owner_session_id) \
         VALUES ('session-x', '/x.jsonl', '/x', 1, 1, 1, 'session-owner')",
    )
    .execute(&state)
    .await;
    let error = result.expect_err("partial ownership must be rejected");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
}
