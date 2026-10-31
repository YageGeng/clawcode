use super::fixture::{Route, TestKernel, finished, request, text};
use kernel::{SessionEventSinks, WhenBusy};
use protocol::{RunRequest, StopReason};
use std::sync::Arc;
use tokio::sync::Notify;

/// A different run queued at the gate must not overwrite an already answered submission.
#[tokio::test]
async fn queued_submission_keeps_its_answer() {
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let test = TestKernel::new(move |r| {
        match r.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("A") => Route::Wait(
                Arc::clone(&gate),
                vec![finished(StopReason::EndTurn)],
            ),
            Some("B") => Route::answer("answer for B"),
            _ => Route::answer("answer for C"),
        }
    });
    let id = test.create("owner").await;
    let kernel = test.kernel.clone();
    let input = request(&id, "A", WhenBusy::FollowUp);
    let a = tokio::spawn(async move { kernel.submit(input).await });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let kernel = test.kernel.clone();
    let input = request(&id, "B", WhenBusy::FollowUp);
    let b = tokio::spawn(async move { kernel.submit(input).await });
    test.wait_until(|| {
        !test
            .kernel
            .pending_messages(&id)
            .expect("pending")
            .follow_up
            .is_empty()
    })
    .await;
    let kernel = test.kernel.clone();
    let sink = test.sinks.sink(&id);
    let input = RunRequest {
        session_id: id.clone(),
        input: "C".into(),
    };
    let c = tokio::spawn(async move { kernel.run(input, sink).await });
    tokio::task::yield_now().await;
    release.notify_one();
    a.await.expect("join A").expect("A");
    let answer = b.await.expect("join B").expect("B");
    c.await.expect("join C").expect("C");
    test.kernel.close_session(&id).await.expect("close");
    assert_eq!(answer.text, "answer for B");
}

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
                    protocol::SessionOwnership::Background {
                        session_id: owner.clone(),
                        name: name.into(),
                    },
                    protocol::SessionTitle::try_from(name).expect("title"),
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

/// A delegated answer should use the host's existing bounded tool projection.
#[tokio::test]
async fn foreground_answer_respects_tool_budget() {
    let test = TestKernel::new(|r| {
        match r.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("delegate") => Route::Events(vec![
                protocol::ModelStreamEvent::ToolCall(
                    protocol::ToolCall::builder()
                        .tool_call_id(
                            protocol::ToolCallId::try_from("delegate-call")
                                .expect("id"),
                        )
                        .name("subagent".into())
                        .arguments(serde_json::json!({"task":"long answer"}))
                        .build(),
                ),
                finished(StopReason::ToolUse),
            ]),
            Some("long answer") => Route::answer(&"x".repeat(30 * 1024)),
            _ => Route::answer("done"),
        }
    });
    let id = test.create("owner").await;
    test.kernel
        .submit(request(&id, "delegate", WhenBusy::FollowUp))
        .await
        .expect("submit");
    let messages = test.kernel.session_transcript(&id).expect("transcript");
    let answer = messages
        .iter()
        .find(|m| {
            matches!(m.content, protocol::MessageContent::ToolResult { .. })
        })
        .expect("tool answer");
    let bytes = answer.content.text_content().expect("text").len();
    test.kernel.close_session(&id).await.expect("close");
    assert!(
        bytes <= tools::TOOL_TEXT_BYTES,
        "foreground copied {bytes} bytes, budget is {}",
        tools::TOOL_TEXT_BYTES
    );
}

/// Polling a cold child's status needs its identity, not readable transcript contents.
#[tokio::test]
async fn cold_runtime_status_does_not_parse_transcript_body() {
    use std::io::Write;
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("owner").await;
    test.kernel.close_session(&session).await.expect("close");
    let factory = store::JsonlStoreFactory::new(
        test.root.path().join("sessions"),
        Arc::new(store::SystemClock),
    );
    let path = store::StoreFactory::list(&factory, None)
        .expect("metadata")
        .first()
        .expect("session")
        .path
        .clone();
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append")
        .write_all(&[0xff])
        .expect("opaque body");
    assert!(
        !test
            .kernel
            .session_runtime(&session)
            .expect("idle status")
            .running
    );
}

/// A malformed ownership cycle fails before awaiting the child's own build reservation.
#[tokio::test]
async fn cyclic_owner_is_rejected_without_waiting_on_itself() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let owner = test.create("owner").await;
    let child = test
        .kernel
        .create_owned_session(
            protocol::SessionOwnership::Background {
                session_id: owner.clone(),
                name: "child".into(),
            },
            protocol::SessionTitle::try_from("child").expect("title"),
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
