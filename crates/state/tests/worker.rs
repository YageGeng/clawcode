mod support;

use std::sync::Arc;

use sqlx::Row;
use state::StateWorker;
use store::{JsonlStoreFactory, StoreFactory, SyncObserver};
use support::{Fixture, SessionLog, TickClock};

/// A store sync reaches the database once the worker drains its queue.
#[tokio::test]
async fn worker_refreshes_after_store_sync() {
    let fixture = Fixture::new().await;
    let (worker, _task) = StateWorker::spawn(Arc::clone(&fixture.runtime));
    let factory = JsonlStoreFactory::new(
        fixture.home.path().join("sessions"),
        Arc::new(TickClock::default()),
    )
    .with_sync_observer(Arc::clone(&worker) as Arc<dyn SyncObserver>);
    let mut log =
        SessionLog::new(factory.create(fixture.options("w1")).expect("create"));
    log.user("queued");
    log.sync();

    worker.flush().await.expect("projected");

    let row =
        sqlx::query("SELECT has_user_message FROM sessions WHERE id = 'w1'")
            .fetch_one(&fixture.state_pool().await)
            .await
            .expect("row");
    assert_eq!(row.get::<i64, _>(0), 1);
}

/// A sync event that outlives its Session's deletion does not recreate the row.
#[tokio::test]
async fn worker_does_not_resurrect_deleted_session() {
    let fixture = Fixture::new().await;
    let (worker, _task) = StateWorker::spawn(Arc::clone(&fixture.runtime));
    let mut log = fixture.session(fixture.options("w2"));
    log.user("soon gone");
    log.sync();
    worker.synced(&log.id(), &log.path());
    worker.flush().await.expect("projected");
    assert!(
        fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .is_some()
    );

    std::fs::remove_file(log.path()).expect("delete log");
    fixture
        .runtime
        .delete_session(&log.id())
        .await
        .expect("delete row");
    worker.synced(&log.id(), &log.path());
    worker.flush().await.expect("projected");

    assert!(
        fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .is_none()
    );
}

/// A flush reports a rejected projection and retries the durable log after the fault clears.
#[tokio::test]
async fn flush_retains_failed_syncs_until_they_are_applied() {
    let fixture = Fixture::new().await;
    let (worker, _task) = StateWorker::spawn(Arc::clone(&fixture.runtime));
    let mut log = fixture.session(fixture.options("rejected"));
    log.user("durable");
    log.sync();
    let pool = fixture.state_pool().await;
    sqlx::query("CREATE TRIGGER reject_insert BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'rejected'); END")
        .execute(&pool).await.expect("reject projection");
    worker.synced(&log.id(), &log.path());
    let error = worker
        .flush()
        .await
        .expect_err("flush must expose the failure");
    assert!(error.to_string().contains("rejected"));
    sqlx::query("DROP TRIGGER reject_insert")
        .execute(&pool)
        .await
        .expect("restore writes");
    worker.flush().await.expect("retry previously failed log");
    assert_eq!(
        fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .expect("row")
            .preview,
        "durable"
    );
}

/// A lock outliving SQLite's busy timeout is retried without another sync or flush.
#[tokio::test]
async fn worker_retries_a_busy_database_after_unlock() {
    let fixture = Fixture::new().await;
    let (worker, _task) = StateWorker::spawn(Arc::clone(&fixture.runtime));
    let mut log = fixture.session(fixture.options("busy"));
    log.user("durable");
    log.sync();
    let pool = fixture.state_pool().await;
    let lock = pool.begin_with("BEGIN IMMEDIATE").await.expect("hold lock");
    worker.synced(&log.id(), &log.path());
    assert!(worker.flush().await.is_err());
    lock.rollback().await.expect("unlock");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .is_none()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("automatic retry");
}
