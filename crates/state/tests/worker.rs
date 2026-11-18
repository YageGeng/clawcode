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

    worker.flush().await;

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
    worker.flush().await;
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
    worker.flush().await;

    assert!(
        fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .is_none()
    );
}
