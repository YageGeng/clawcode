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
    let runtime = std::sync::Arc::clone(&fixture.runtime);
    let factory = std::sync::Arc::clone(&fixture.factory);
    let sync = tokio::spawn(async move { runtime.sync_logs(&factory).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!fixture.runtime.is_ready());

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
