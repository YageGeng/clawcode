//! Kernel metadata lookups served by the SQLite state catalog.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kernel::KernelError;
use protocol::{SessionId, SessionOwnership, SessionTitle};
use store::{
    Clock, JsonlStoreFactory, OwnedSessionCreateOptions, SessionCreateOptions,
    SessionForkOptions, SessionHeader, SessionMetadata, SessionStore,
    StoreError, StoreFactory,
};

use super::agents::{agents_with_state, call, child, spawn};
use super::fixture::{Route, TestKernel};

/// Builds the production JSONL store under the fixture root.
fn jsonl(root: &Path, clock: Arc<dyn Clock>) -> Arc<dyn StoreFactory> {
    Arc::new(JsonlStoreFactory::new(root.join("sessions"), clock))
}

/// JSONL store that counts every metadata scan it performs.
struct CountingStoreFactory {
    inner: JsonlStoreFactory,
    scans: Arc<AtomicUsize>,
}

impl CountingStoreFactory {
    /// Records one scan.
    fn scanned(&self) {
        self.scans.fetch_add(1, Ordering::SeqCst);
    }
}

impl StoreFactory for CountingStoreFactory {
    /// Counts a header scan, then delegates.
    fn headers(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionHeader>, StoreError> {
        self.scanned();
        self.inner.headers(cwd)
    }
    /// Counts a single-header scan, then delegates.
    fn header(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionHeader>, StoreError> {
        self.scanned();
        self.inner.header(session_id)
    }
    /// Counts an existence scan, then delegates.
    fn exists(&self, session_id: &SessionId) -> Result<bool, StoreError> {
        self.scanned();
        self.inner.exists(session_id)
    }
    /// Counts a listing scan, then delegates.
    fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionMetadata>, StoreError> {
        self.scanned();
        self.inner.list(cwd)
    }
    /// Delegates owned creation without counting.
    fn create_owned(
        &self,
        options: OwnedSessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        self.inner.create_owned(options)
    }
    /// Delegates creation without counting.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        self.inner.create(options)
    }
    /// Delegates opening without counting.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        self.inner.open(path)
    }
    /// Delegates forking without counting.
    fn fork(
        &self,
        source_path: &Path,
        options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        self.inner.fork(source_path, options)
    }
    /// Delegates deletion without counting.
    fn delete(&self, session_id: &SessionId) -> Result<(), StoreError> {
        self.inner.delete(session_id)
    }
    /// Delegates renaming without counting.
    fn set_name_at(
        &self,
        path: &Path,
        name: Option<String>,
    ) -> Result<(), StoreError> {
        self.inner.set_name_at(path, name)
    }
}

/// Builds a Kernel with ready SQLite state and the production JSONL store.
async fn ready() -> TestKernel {
    TestKernel::with_state(|_| Route::answer("ok"), jsonl, true).await
}

/// Reports whether the Kernel lists `id`.
async fn listed(test: &TestKernel, id: &SessionId) -> bool {
    test.kernel
        .list_sessions(None)
        .await
        .expect("list")
        .iter()
        .any(|summary| &summary.session_id == id)
}

/// A new Session is in SQLite and listed as soon as creation returns.
#[tokio::test]
async fn created_session_lists_immediately() {
    let test = ready().await;
    let id = test.create("session-a").await;

    let state = test.state.as_ref().expect("state");
    assert!(state.session(&id).await.expect("row").is_some());
    assert!(listed(&test, &id).await);
    let summary = test.kernel.session_summary(&id).await.expect("summary");
    assert_eq!(summary.map(|summary| summary.session_id), Some(id));
}

/// A title change is visible to the next listing.
#[tokio::test]
async fn renamed_session_lists_new_name_immediately() {
    let test = ready().await;
    let id = test.create("session-r").await;
    test.kernel
        .set_session_title(
            &id,
            SessionTitle::try_from("Renamed").expect("title"),
        )
        .await
        .expect("rename");

    let summary = test
        .kernel
        .list_sessions(None)
        .await
        .expect("list")
        .into_iter()
        .find(|summary| summary.session_id == id)
        .expect("listed");
    assert_eq!(summary.name.as_deref(), Some("Renamed"));
}

/// A deleted Session leaves the listing and its runtime lookups.
#[tokio::test]
async fn deleted_session_disappears() {
    let test = ready().await;
    let id = test.create("session-d").await;
    test.kernel.delete_session(&id).await.expect("delete");

    assert!(!listed(&test, &id).await);
    assert!(matches!(
        test.kernel.session_runtime(&id).await,
        Err(KernelError::SessionNotFound(_))
    ));
    let state = test.state.as_ref().expect("state");
    assert!(state.session(&id).await.expect("lookup").is_none());
}

/// Children recover their shared owner through SQLite owner lookups.
#[tokio::test]
async fn parallel_children_resume_shared_owner_with_state() {
    let test = ready().await;
    let owner = test.create("owner").await;
    let mut children = Vec::new();
    for name in ["one", "two"] {
        children.push(
            test.kernel
                .create_owned_session(
                    kernel::OwnedSessionRequest::builder()
                        .ownership(SessionOwnership {
                            session_id: owner.clone(),
                            name: protocol::AgentName::try_from(name)
                                .expect("name"),
                            nickname: name.into(),
                        })
                        .title(SessionTitle::try_from(name).expect("title"))
                        .model(None)
                        .build(),
                )
                .await
                .expect("child"),
        );
    }
    test.kernel.close_session(&owner).await.expect("close");
    let cwd = test.root.path().to_path_buf();

    let (first, second) = tokio::join!(
        test.kernel.resume_session(children[0].clone(), cwd.clone()),
        test.kernel.resume_session(children[1].clone(), cwd),
    );

    first.expect("first child");
    second.expect("second child");
    test.kernel
        .close_session(&owner)
        .await
        .expect("close owner");
}

/// A resumed owner's roster comes from SQLite, not from header scans.
#[tokio::test]
async fn roster_loads_from_state_without_header_scans() {
    let scans = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&scans);
    let test = agents_with_state(
        |_| Route::answer("done"),
        move |root, clock| {
            Arc::new(CountingStoreFactory {
                inner: JsonlStoreFactory::new(root.join("sessions"), clock),
                scans: counter,
            })
        },
    )
    .await;
    let owner = test.create("owner").await;
    spawn(
        &test,
        &owner,
        serde_json::json!([["s1", {"task_name": "reader", "message": "Read.", "fork_turns": "none"}]]),
    )
    .await;
    test.wait_until(async || !test.agent_mails(&owner).is_empty())
        .await;
    test.kernel.close_session(&owner).await.expect("close");
    let before = scans.load(Ordering::SeqCst);

    test.kernel
        .resume_session(owner.clone(), test.root.path().into())
        .await
        .expect("resume");
    let reply = call(
        &test,
        &owner,
        serde_json::json!([["l", protocol::LIST_AGENTS_TOOL, {"path_prefix": "/root/reader"}]]),
    )
    .await
    .remove(0);

    let listed: serde_json::Value =
        serde_json::from_str(&reply).expect("list JSON");
    assert_eq!(listed["agents"][0]["agent_name"], "/root/reader");
    assert_eq!(
        child(&test, &owner, "reader")
            .await
            .ownership
            .expect("edge")
            .name
            .as_ref(),
        "reader"
    );
    assert_eq!(
        scans.load(Ordering::SeqCst),
        before,
        "resume and list must not scan headers"
    );
}

/// Before the backfill finishes, lookups still find Sessions by scanning.
#[tokio::test]
async fn catalog_scans_until_backfill_completes() {
    let test =
        TestKernel::with_state(|_| Route::answer("ok"), jsonl, false).await;
    let id = test.create("session-s").await;
    assert!(!test.state.as_ref().expect("state").is_ready());
    assert!(listed(&test, &id).await);
}

/// A row whose log vanished is removed on the next lookup.
#[tokio::test]
async fn missing_file_row_is_repaired_on_lookup() {
    let test = ready().await;
    let id = test.create("session-m").await;
    test.kernel.close_session(&id).await.expect("close");
    let state = test.state.as_ref().expect("state");
    let path = state.session(&id).await.expect("lookup").expect("row").path;
    std::fs::remove_file(path).expect("remove log");

    assert!(matches!(
        test.kernel.session_runtime(&id).await,
        Err(KernelError::SessionNotFound(_))
    ));
    assert!(state.session(&id).await.expect("lookup").is_none());
}
