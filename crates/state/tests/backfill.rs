mod support;

use std::path::PathBuf;

use protocol::SessionId;
use sqlx::Row;
use state::BackfillOutcome;
use support::{Fixture, SessionLog};

/// Writes three Sessions with known token totals and returns their logs.
fn three_sessions(fixture: &Fixture) -> Vec<SessionLog> {
    ["b1", "b2", "b3"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let mut log = fixture.session(fixture.options(id));
            log.user("hello");
            log.assistant(
                "p",
                "m",
                10 * (u64::try_from(index).expect("index") + 1),
            );
            log.sync();
            log
        })
        .collect()
}

/// Runs the backfill over every log under the fixture's sessions root.
async fn backfill(fixture: &Fixture) -> BackfillOutcome {
    let paths = fixture.factory.session_paths().expect("paths");
    fixture
        .runtime
        .backfill(fixture.factory.sessions_root(), &paths)
        .await
        .expect("backfill")
}

/// Reads `(status, last_watermark)` of the backfill row.
async fn backfill_row(fixture: &Fixture) -> (String, Option<String>) {
    let row = sqlx::query(
        "SELECT status, last_watermark FROM backfill_state WHERE id = 1",
    )
    .fetch_one(&fixture.state_pool().await)
    .await
    .expect("backfill row");
    (row.get(0), row.get(1))
}

/// Reads every Session's token total, ordered by id.
async fn token_totals(fixture: &Fixture) -> Vec<(String, i64)> {
    sqlx::query_as("SELECT id, tokens_used FROM sessions ORDER BY id")
        .fetch_all(&fixture.state_pool().await)
        .await
        .expect("tokens")
}

/// A first backfill projects every log and marks the runtime ready.
#[tokio::test]
async fn backfill_projects_every_file_and_completes() {
    let fixture = Fixture::new().await;
    three_sessions(&fixture);

    assert_eq!(
        backfill(&fixture).await,
        BackfillOutcome::Completed {
            scanned: 3,
            projected: 3,
            failed: 0
        }
    );
    assert!(fixture.runtime.is_ready());
    assert_eq!(backfill_row(&fixture).await.0, "complete");
    assert_eq!(
        token_totals(&fixture).await,
        [("b1".into(), 10), ("b2".into(), 20), ("b3".into(), 30)]
    );
    assert_eq!(backfill(&fixture).await, BackfillOutcome::AlreadyComplete);
}

/// A fresh lease held by another process stops a second worker.
#[tokio::test]
async fn backfill_skips_when_lease_is_held() {
    let fixture = Fixture::new().await;
    three_sessions(&fixture);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("now");
    sqlx::query("UPDATE backfill_state SET status = 'running', updated_at_ms = ? WHERE id = 1")
        .bind(now)
        .execute(&fixture.state_pool().await)
        .await
        .expect("hold lease");

    assert_eq!(backfill(&fixture).await, BackfillOutcome::LeaseHeld);
    assert!(token_totals(&fixture).await.is_empty());
    assert!(!fixture.runtime.is_ready());
}

/// Startup sync waits out another process's lease and then becomes ready.
#[tokio::test]
async fn sync_becomes_ready_after_held_lease_completes() {
    let fixture = Fixture::new().await;
    three_sessions(&fixture);
    let pool = fixture.state_pool().await;
    sqlx::query("UPDATE backfill_state SET status = 'running', updated_at_ms = ? WHERE id = 1")
        .bind(i64::MAX)
        .execute(&pool)
        .await
        .expect("hold lease");
    // Enumerating this root would fail immediately while the peer holds its lease.
    let root = fixture.factory.sessions_root();
    let parked = fixture.home.path().join("parked-logs");
    std::fs::rename(root, &parked).expect("park sessions");
    std::fs::write(root, b"unavailable").expect("unlistable root");
    let runtime = std::sync::Arc::clone(&fixture.runtime);
    let factory = std::sync::Arc::clone(&fixture.factory);
    let sync = tokio::spawn(async move { runtime.sync_logs(&factory).await });
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(!fixture.runtime.is_ready());
    assert!(
        !sync.is_finished(),
        "lease polling attempted to enumerate the root"
    );
    std::fs::remove_file(root).expect("restore directory");
    std::fs::rename(parked, root).expect("restore logs");

    sqlx::query("UPDATE backfill_state SET status = 'complete' WHERE id = 1")
        .execute(&pool)
        .await
        .expect("holder completes");
    tokio::time::timeout(std::time::Duration::from_secs(5), sync)
        .await
        .expect("sync finishes")
        .expect("sync task")
        .expect("sync");

    assert!(fixture.runtime.is_ready());
}

/// Startup sync repairs logs created, grown, or removed while SQLite was not told.
#[tokio::test]
async fn sync_reconciles_logs_changed_behind_its_back() {
    let fixture = Fixture::new().await;
    let mut logs = three_sessions(&fixture);
    assert!(matches!(
        backfill(&fixture).await,
        BackfillOutcome::Completed { .. }
    ));
    let mut fresh = fixture.session(fixture.options("b4"));
    fresh.user("created by another binary");
    fresh.sync();
    let mut grown = logs.remove(0);
    grown.assistant("p", "m", 5);
    grown.sync();
    std::fs::remove_file(logs.remove(0).path()).expect("remove b2");

    fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("sync");

    assert_eq!(
        token_totals(&fixture).await,
        [("b1".into(), 15), ("b3".into(), 30), ("b4".into(), 0)]
    );
}

/// Resuming from a watermark after a lost lease never re-counts tokens.
#[tokio::test]
async fn backfill_resumes_after_expired_lease_without_double_counting() {
    let fixture = Fixture::new().await;
    three_sessions(&fixture);
    backfill(&fixture).await;
    let single_run = token_totals(&fixture).await;
    let first = fixture.factory.session_paths().expect("paths")[0]
        .strip_prefix(fixture.factory.sessions_root())
        .expect("relative")
        .to_str()
        .expect("utf8")
        .to_owned();
    sqlx::query(
        "UPDATE backfill_state SET status = 'running', last_watermark = ?, updated_at_ms = 0 WHERE id = 1",
    )
    .bind(&first)
    .execute(&fixture.state_pool().await)
    .await
    .expect("expired lease");

    assert_eq!(
        backfill(&fixture).await,
        BackfillOutcome::Completed {
            scanned: 2,
            projected: 2,
            failed: 0
        }
    );
    assert_eq!(token_totals(&fixture).await, single_run);
}

/// A copy of a log under another cwd keeps the first-sorted path.
#[tokio::test]
async fn backfill_skips_duplicate_id_at_other_path() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("dup"));
    log.user("original");
    log.sync();
    let copy_dir = fixture.factory.sessions_root().join("--zz-copy--");
    std::fs::create_dir_all(&copy_dir).expect("copy dir");
    let copy: PathBuf = copy_dir.join(log.path().file_name().expect("name"));
    std::fs::copy(log.path(), &copy).expect("copy");
    let first = fixture.factory.session_paths().expect("paths")[0].clone();

    assert_eq!(
        backfill(&fixture).await,
        BackfillOutcome::Completed {
            scanned: 2,
            projected: 1,
            failed: 0
        }
    );
    let row = fixture
        .runtime
        .session(&SessionId::try_from("dup").expect("id"))
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(row.path, first);
}

/// An unreadable log is counted and does not stop the backfill.
#[tokio::test]
async fn backfill_counts_unreadable_file_as_failed() {
    let fixture = Fixture::new().await;
    three_sessions(&fixture);
    let directory = fixture.factory.sessions_root().join("--broken--");
    std::fs::create_dir_all(&directory).expect("dir");
    std::fs::write(directory.join("1_broken.jsonl"), "garbage\n")
        .expect("garbage");

    assert_eq!(
        backfill(&fixture).await,
        BackfillOutcome::Completed {
            scanned: 4,
            projected: 3,
            failed: 1
        }
    );
    assert!(fixture.runtime.is_ready());
}

/// A rebuilt history database must catch up even when the state file survived intact.
#[tokio::test]
async fn sync_repairs_a_missing_history_cursor() {
    let fixture = Fixture::new().await;
    three_sessions(&fixture);
    backfill(&fixture).await;
    // Keep only a consistent metadata snapshot, then let startup recreate history from scratch.
    let recovered =
        state::SqliteConfig::new(fixture.home.path().join("recovered"));
    std::fs::create_dir(recovered.home()).expect("recovery directory");
    sqlx::query("VACUUM INTO ?")
        .bind(recovered.state_db_path().to_str().expect("path"))
        .execute(&fixture.state_pool().await)
        .await
        .expect("metadata snapshot");
    let runtime = state::StateRuntime::init(&recovered).await.expect("reopen");
    assert!(!runtime.is_ready());
    runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("reconcile");
    let history = support::external(&recovered.session_history_db_path()).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM entries")
        .fetch_one(&history)
        .await
        .expect("external reader");
    assert_eq!(count, 6);
}

/// The startup catalog stays on JSONL until the identity of every readable log is indexed.
#[tokio::test]
async fn sync_reports_failed_identity_writes_until_repaired() {
    let fixture = Fixture::new().await;
    backfill(&fixture).await;
    let log = fixture.session(fixture.options("waiting"));
    let pool = fixture.state_pool().await;
    sqlx::query("CREATE TRIGGER reject_header BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'unavailable'); END")
        .execute(&pool).await.expect("reject header");
    let error = fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect_err("rejected header");
    assert!(matches!(error, state::StateError::Projection { .. }));
    assert!(!fixture.runtime.is_ready());
    sqlx::query("DROP TRIGGER reject_header")
        .execute(&pool)
        .await
        .expect("restore writes");
    fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("repair");
    assert!(fixture.runtime.is_ready());
    assert!(
        fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .is_some()
    );
}

/// A restart after the metadata half of a deletion clears any orphaned history.
#[tokio::test]
async fn sync_finishes_a_partially_committed_deletion() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("removed"));
    log.user("remove");
    log.sync();
    backfill(&fixture).await;
    std::fs::remove_file(log.path()).expect("remove log");
    sqlx::query("DELETE FROM sessions")
        .execute(&fixture.state_pool().await)
        .await
        .expect("remove metadata");
    fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("reconcile");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM entries")
        .fetch_one(&fixture.history_pool().await)
        .await
        .expect("history");
    assert_eq!(count, 0);
}

/// A moved log replaces its missing canonical path within the same reconciliation.
#[tokio::test]
async fn sync_adopts_a_moved_log_in_one_pass() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("moved"));
    log.user("keep");
    log.sync();
    let id = log.id();
    let old = log.path();
    drop(log);
    backfill(&fixture).await;
    let moved = old.with_file_name("imported.jsonl");
    std::fs::rename(old, &moved).expect("rename log");
    fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("reconcile");
    let row = fixture
        .runtime
        .session(&id)
        .await
        .expect("query")
        .expect("moved session");
    assert_eq!(row.path, moved);
    assert_eq!(row.preview, "keep");
}

/// Closing a waiter preserves its peer's lease; closing the owner immediately permits takeover.
#[tokio::test]
async fn graceful_shutdown_only_releases_the_owned_lease() {
    use std::sync::Arc;
    use std::time::Duration;
    let fixture = Fixture::new().await;
    three_sessions(&fixture);
    let pool = fixture.state_pool().await;
    let history = fixture.history_pool().await;
    let lock = history
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("pause projection");
    let start = || {
        state::StartupBackfill::spawn(
            Arc::clone(&fixture.runtime),
            Arc::clone(&fixture.factory),
        )
    };
    let owner = start();
    let token = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let token: Option<String> =
                sqlx::query_scalar("SELECT lease_owner FROM backfill_state")
                    .fetch_one(&pool)
                    .await
                    .expect("lease");
            if let Some(token) = token {
                break token;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owner acquired lease");
    let waiter = start();
    tokio::time::sleep(Duration::from_millis(30)).await;
    waiter.shutdown().await.expect("close waiter");
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT lease_owner FROM backfill_state"
        )
        .fetch_one(&pool)
        .await
        .expect("original owner"),
        token
    );
    lock.rollback().await.expect("unlock");
    owner.shutdown().await.expect("close owner");
    let (status, token): (String, Option<String>) =
        sqlx::query_as("SELECT status, lease_owner FROM backfill_state")
            .fetch_one(&pool)
            .await
            .expect("released");
    assert_eq!((status.as_str(), token), ("pending", None));
    fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("resume immediately");
    assert_eq!(token_totals(&fixture).await.len(), 3);
}

/// Startup adopts an old relative catalog path even if it resolves to a file in the current cwd.
#[tokio::test]
async fn reconciliation_replaces_legacy_relative_paths_in_both_databases() {
    let fixture = Fixture::new().await;
    let log = fixture.session(fixture.options("relative"));
    backfill(&fixture).await;
    let state = fixture.state_pool().await;
    let history = fixture.history_pool().await;
    sqlx::query("UPDATE sessions SET path = 'Cargo.toml'")
        .execute(&state)
        .await
        .expect("legacy metadata");
    sqlx::query("UPDATE projection_state SET path = 'Cargo.toml'")
        .execute(&history)
        .await
        .expect("legacy history");
    fixture
        .runtime
        .sync_logs(&fixture.factory)
        .await
        .expect("adopt absolute path");
    let row = fixture
        .runtime
        .session(&log.id())
        .await
        .expect("read")
        .expect("row");
    assert!(row.path.is_absolute());
    assert_eq!(row.path, log.path());
    let path: String = sqlx::query_scalar("SELECT path FROM projection_state")
        .fetch_one(&history)
        .await
        .expect("history path");
    assert_eq!(PathBuf::from(path), log.path());
}

/// A relative spelling of the same root must not turn its logs into duplicate Sessions.
#[tokio::test]
async fn alternate_root_spelling_reconciles_the_same_log() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("alias"));
    log.name(Some("first"));
    log.sync();
    backfill(&fixture).await;
    let path = log.path();
    let original = std::fs::read_to_string(&path).expect("log");
    std::fs::write(&path, original.replace("first", "other")).expect("rewrite");
    let root = fixture.factory.sessions_root().join("..").join("sessions");
    let alias = std::sync::Arc::new(store::JsonlStoreFactory::new(
        root,
        std::sync::Arc::new(store::SystemClock),
    ));
    fixture
        .runtime
        .sync_logs(&alias)
        .await
        .expect("reconcile alias");
    let row = fixture
        .runtime
        .session(&log.id())
        .await
        .expect("lookup")
        .expect("session");
    assert_eq!(row.name.as_deref(), Some("other"));
}
