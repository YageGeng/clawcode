use super::fixture::{Route, TestKernel};

/// Cascading deletion works with closed descendants without scanning unrelated transcript bodies.
#[tokio::test]
async fn deleting_owner_only_reads_headers_for_ownership() {
    use std::io::Write;
    use store::StoreFactory;
    let test = TestKernel::new(|_| Route::answer("ok"));
    let owner = test.create("owner").await;
    let child = test
        .kernel
        .create_owned_session(
            ownership(&owner),
            SessionTitle::try_from("child").expect("title"),
        )
        .await
        .expect("child");
    test.kernel.close_session(&owner).await.expect("close");
    let factory = store::JsonlStoreFactory::new(
        test.root.path().join("sessions"),
        std::sync::Arc::new(store::SystemClock),
    );
    let path = factory
        .header(&owner)
        .expect("owner lookup")
        .expect("owner header")
        .path;
    let imported = path.with_file_name("imported-owner.jsonl");
    std::fs::rename(path, &imported).expect("rename owner log");
    let other = SessionId::try_from("unrelated").expect("id");
    let store = factory
        .create(store::SessionCreateOptions {
            session_id: other.clone(),
            cwd: test.root.path().join("another-project"),
            parent_session_id: None,
        })
        .expect("other");
    let path = store.path().to_path_buf();
    drop(store);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append")
        .write_all(&[0xff])
        .expect("opaque body");
    test.kernel.delete_session(&owner).await.expect("cascade");
    assert!(!factory.exists(&owner).expect("owner absent"));
    assert!(!imported.exists());
    assert!(!factory.exists(&child).expect("child absent"));
    assert!(factory.exists(&other).expect("unrelated preserved"));
}

/// Suspends child startup after its runtime has been registered.
#[derive(Clone)]
struct ChildStartupGate {
    started: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl extension::ExtensionHandler<extension::SessionStartPoint>
    for ChildStartupGate
{
    /// Leaves the owner startup immediate and gates only delegated Sessions.
    async fn handle(
        &self,
        _event: &protocol::SessionStartEvent,
        context: &extension::ExtensionContext,
    ) -> Result<(), extension::ExtensionError> {
        if context.invocation.session_id.as_str() != "owner" {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}
impl extension::ExtensionModule for ChildStartupGate {
    /// Identifies the lifecycle-race extension fixture.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("child-gate").expect("id"),
            name: "Child gate".into(),
            version: "1".into(),
        }
    }
    /// Installs the shared child-start gate through the production event registrar.
    fn register(
        &self,
        registrar: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        registrar.on::<extension::SessionStartPoint, _>(self.clone())
    }
}

/// Closing an owner waits for an externally initiated child creation before cascading.
#[tokio::test]
async fn close_waits_for_inflight_owned_creation() {
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate = ChildStartupGate {
        started: std::sync::Arc::clone(&started),
        release: std::sync::Arc::clone(&release),
    };
    let test = TestKernel::with_extensions(
        |_| Route::answer("ok"),
        std::sync::Arc::new(extension::StaticExtensionFactory::new(
            Default::default(),
            vec![std::sync::Arc::new(move || {
                Ok(std::sync::Arc::new(gate.clone()))
            })],
        )),
    );
    let owner = test.create("owner").await;
    let kernel = test.kernel.clone();
    let edge = ownership(&owner);
    let creating = tokio::spawn(async move {
        kernel
            .create_owned_session(
                edge,
                SessionTitle::try_from("child").expect("title"),
            )
            .await
    });
    started.notified().await;
    let kernel = test.kernel.clone();
    let id = owner.clone();
    let closing = tokio::spawn(async move { kernel.close_session(&id).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    release.notify_one();
    let child = creating.await.expect("create join").expect("child");
    closing.await.expect("close join").expect("close owner");
    assert!(matches!(
        test.kernel.session_transcript(&child),
        Err(KernelError::SessionNotFound(_))
    ));
}
use kernel::KernelError;
use protocol::{SessionId, SessionOwnership, SessionTitle, ToolCallId};

/// Builds a foreground ownership edge using validated protocol identifiers.
pub fn ownership(owner: &SessionId) -> SessionOwnership {
    SessionOwnership::Tool {
        session_id: owner.clone(),
        tool_call_id: ToolCallId::try_from("call-1").expect("call"),
    }
}

/// New children persist the owner's settings and exclude delegation tools before startup.
#[tokio::test]
async fn owned_session_copies_owner_agent() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let owner = test.create("owner").await;
    test.kernel
        .invoke_extension_command(
            &owner,
            "agent-state".into(),
            serde_json::json!({"action":"set"}),
        )
        .await
        .expect("settings");
    let child = test
        .kernel
        .create_owned_session(
            ownership(&owner),
            SessionTitle::try_from("subagent: x").expect("title"),
        )
        .await
        .expect("child");
    let children = test.owned_children(&owner);
    let summary = children.first().expect("child summary");
    assert_eq!(summary.ownership, Some(ownership(&owner)));
    assert_eq!(summary.name.as_deref(), Some("subagent: x"));
    let tree = test.kernel.session_tree(&child).expect("tree");
    for (kind, payload) in [
        (
            "model_change",
            serde_json::json!({"provider":"fixture", "modelId":"secondary"}),
        ),
        (
            "thinking_level_change",
            serde_json::json!({"thinkingLevel":"high"}),
        ),
    ] {
        assert_eq!(
            tree.entries
                .iter()
                .rev()
                .find(|e| e.kind == kind)
                .expect("setting")
                .payload,
            payload
        );
    }
    let policy = &tree
        .entries
        .iter()
        .rev()
        .find(|e| e.kind == "active_tools_change")
        .expect("tools")
        .payload["selection"];
    for name in ["subagent", "background_subagent"] {
        assert!(
            policy["disabled"]
                .as_array()
                .expect("disabled")
                .contains(&name.into())
        );
    }
}

/// A missing owner cannot create a disconnected persisted child.
#[tokio::test]
async fn creating_owned_session_requires_live_owner() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let missing = SessionId::try_from("missing").expect("id");
    assert!(matches!(
        test.kernel
            .create_owned_session(
                ownership(&missing),
                SessionTitle::try_from("x").expect("title")
            )
            .await,
        Err(KernelError::OwnerUnavailable(_))
    ));
    assert!(test.kernel.list_sessions(None).expect("list").is_empty());
}
