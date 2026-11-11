use super::fixture::{Route, TestKernel};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone)]
struct ResumeGate {
    started: Arc<Notify>,
    release: Arc<Notify>,
}
impl extension::ExtensionModule for ResumeGate {
    /// Identifies the owner-start gate used by the cold-recovery test.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("gate").expect("id"),
            name: "Gate".into(),
            version: "1".into(),
        }
    }
    /// Runs the gate through the real startup extension boundary.
    fn register(
        &self,
        r: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        r.on::<extension::SessionStartPoint, _>(self.clone())
    }
}
#[async_trait::async_trait]
impl extension::ExtensionHandler<extension::SessionStartPoint> for ResumeGate {
    /// Holds only the owner's cold startup while a sibling recovery arrives.
    async fn handle(
        &self,
        e: &protocol::SessionStartEvent,
        c: &extension::ExtensionContext,
    ) -> Result<(), extension::ExtensionError> {
        if e.reason == protocol::SessionStartReason::Resume
            && c.invocation.session_id.as_str() == "owner"
        {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// Parallel child recovery should share the pending owner startup rather than reject a sibling.
#[tokio::test]
async fn parallel_children_resume_shared_owner() {
    let gate = ResumeGate {
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let module = gate.clone();
    let test = TestKernel::with_extensions(
        |_| Route::answer("ok"),
        Arc::new(extension::StaticExtensionFactory::new(
            Default::default(),
            vec![Arc::new(move || Ok(Arc::new(module.clone())))],
        )),
    );
    let owner = test.create("owner").await;
    let mut children = Vec::new();
    for name in ["one", "two"] {
        children.push(
            test.kernel
                .create_owned_session(
                    kernel::OwnedSessionRequest::builder()
                        .ownership(protocol::SessionOwnership {
                            session_id: owner.clone(),
                            name: protocol::AgentName::try_from(name)
                                .expect("name"),
                            nickname: name.into(),
                        })
                        .title(
                            protocol::SessionTitle::try_from(name)
                                .expect("title"),
                        )
                        .model(None)
                        .build(),
                )
                .await
                .expect("child"),
        );
    }
    test.kernel.close_session(&owner).await.expect("close");
    let kernel = test.kernel.clone();
    let id = children[0].clone();
    let cwd = test.root.path().to_path_buf();
    let first =
        tokio::spawn(async move { kernel.resume_session(id, cwd).await });
    gate.started.notified().await;
    let kernel = test.kernel.clone();
    let id = children[1].clone();
    let cwd = test.root.path().to_path_buf();
    let second =
        tokio::spawn(async move { kernel.resume_session(id, cwd).await });
    tokio::task::yield_now().await;
    gate.release.notify_one();
    first.await.expect("join first").expect("first");
    let result = second.await.expect("join second");
    test.kernel.close_session(&owner).await.expect("close");
    assert!(result.is_ok(), "second child resume failed: {result:?}");
}

/// A malformed ownership cycle fails before awaiting the child's own build reservation.
#[tokio::test]
async fn cyclic_owner_is_rejected_without_waiting_on_itself() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let owner = test.create("owner").await;
    let child = test
        .kernel
        .create_owned_session(
            kernel::OwnedSessionRequest::builder()
                .ownership(protocol::SessionOwnership {
                    session_id: owner.clone(),
                    name: protocol::AgentName::try_from("child").expect("name"),
                    nickname: "Euclid".into(),
                })
                .title(
                    protocol::SessionTitle::try_from("child").expect("title"),
                )
                .model(None)
                .build(),
        )
        .await
        .expect("child");
    test.kernel.close_session(&owner).await.expect("close");
    let factory = store::JsonlStoreFactory::new(
        test.root.path().join("sessions"),
        Arc::new(store::SystemClock),
    );
    let path = store::StoreFactory::list(&factory, None)
        .expect("metadata")
        .into_iter()
        .find(|entry| entry.id == child)
        .expect("child log")
        .path;
    let content = std::fs::read_to_string(&path).expect("log");
    let (header, rest) = content.split_once('\n').expect("header line");
    let mut header: serde_json::Value =
        serde_json::from_str(header).expect("header");
    header["ownership"]["sessionId"] = child.as_str().into();
    std::fs::write(path, format!("{header}\n{rest}")).expect("corrupt edge");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        test.kernel.resume_session(child, test.root.path().into()),
    )
    .await
    .expect("no deadlock");
    assert!(
        matches!(result, Err(kernel::KernelError::Protocol(message)) if message.contains("cyclic session ownership"))
    );
}
