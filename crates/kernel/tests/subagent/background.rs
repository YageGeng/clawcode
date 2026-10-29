use super::fixture::{Route, TestKernel, finished, request, text};
use kernel::WhenBusy;
use protocol::{
    MessageContent, ModelStreamEvent, SessionId, StopReason, ToolCall,
    ToolCallId,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Preserves real JSONL storage while forcing a legal scheduler yield before owned creation.
struct SchedulingStore {
    inner: store::JsonlStoreFactory,
    enabled: Arc<std::sync::atomic::AtomicBool>,
    probes: std::sync::atomic::AtomicUsize,
}

impl SchedulingStore {
    /// Exhausts two lookup polls so the following Tokio read lock must yield cooperatively.
    fn yield_after_lookup(&self) {
        use std::sync::atomic::Ordering;
        if !self.enabled.load(Ordering::SeqCst)
            || self.probes.fetch_add(1, Ordering::SeqCst) >= 2
        {
            return;
        }
        let mut cx =
            std::task::Context::from_waker(futures::task::noop_waker_ref());
        for _ in 0..256 {
            match tokio::task::coop::poll_proceed(&mut cx) {
                std::task::Poll::Ready(permit) => permit.made_progress(),
                std::task::Poll::Pending => break,
            }
        }
    }
}

impl store::StoreFactory for SchedulingStore {
    /// Delegates plain creation to the real writer.
    fn create(
        &self,
        options: store::SessionCreateOptions,
    ) -> Result<Box<dyn store::SessionStore>, store::StoreError> {
        self.inner.create(options)
    }
    /// Delegates owned creation without changing persistence order.
    fn create_owned(
        &self,
        options: store::OwnedSessionCreateOptions,
    ) -> Result<Box<dyn store::SessionStore>, store::StoreError> {
        self.inner.create_owned(options)
    }
    /// Preserves normal transcript opening.
    fn open(
        &self,
        path: &std::path::Path,
    ) -> Result<Box<dyn store::SessionStore>, store::StoreError> {
        self.inner.open(path)
    }
    /// Preserves normal branch forking.
    fn fork(
        &self,
        path: &std::path::Path,
        options: store::SessionForkOptions,
    ) -> Result<Box<dyn store::SessionStore>, store::StoreError> {
        self.inner.fork(path, options)
    }
    /// Injects scheduling only after the actual name lookup has finished.
    fn list(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<store::SessionMetadata>, store::StoreError> {
        let sessions = self.inner.list(cwd)?;
        self.yield_after_lookup();
        Ok(sessions)
    }
    /// Injects the same scheduling boundary when name lookup uses lightweight headers.
    fn headers(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<store::SessionHeader>, store::StoreError> {
        let headers = self.inner.headers(cwd)?;
        self.yield_after_lookup();
        Ok(headers)
    }
    /// Locates a single header without exhausting a spawn lookup's budget.
    fn header(
        &self,
        id: &SessionId,
    ) -> Result<Option<store::SessionHeader>, store::StoreError> {
        self.inner.header(id)
    }
    /// Keeps identity lookup independent of scheduling injection.
    fn exists(&self, id: &SessionId) -> Result<bool, store::StoreError> {
        self.inner.exists(id)
    }
    /// Uses normal file deletion for fixture cleanup.
    fn delete(&self, id: &SessionId) -> Result<(), store::StoreError> {
        self.inner.delete(id)
    }
}

/// Same-name calls remain unique when creation yields after both names could have been checked.
#[tokio::test]
async fn concurrent_same_name_spawn_survives_cooperative_yield() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let enabled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&enabled);
    let test = TestKernel::with_store(
        |request| {
            if request.messages.last().is_some_and(|message| {
                text(&message.content) == "delegate twice"
            }) {
                let mut events = ["one", "two"].into_iter().map(|id| ModelStreamEvent::ToolCall(ToolCall {
                tool_call_id: ToolCallId::try_from(id).expect("id"), name: "background_subagent".into(),
                arguments: serde_json::json!({"action":"spawn","name":"reader","message":"answer"}),
            })).collect::<Vec<_>>();
                events.push(finished(StopReason::ToolUse));
                Route::Events(events)
            } else {
                Route::answer("done")
            }
        },
        move |root, clock| {
            Arc::new(SchedulingStore {
                inner: store::JsonlStoreFactory::new(
                    root.join("sessions"),
                    clock,
                ),
                enabled: signal,
                probes: AtomicUsize::new(0),
            })
        },
    );
    let owner = test.create("owner").await;
    enabled.store(true, Ordering::SeqCst);
    let kernel = test.kernel.clone();
    let input = request(&owner, "delegate twice", WhenBusy::FollowUp);
    tokio::spawn(async move { kernel.submit(input).await })
        .await
        .expect("task")
        .expect("run");
    enabled.store(false, Ordering::SeqCst);
    let children = test.owned_children(&owner);
    let messages = test.kernel.session_transcript(&owner).expect("transcript");
    test.kernel.close_session(&owner).await.expect("close");
    assert_eq!(children.len(), 1, "same owner/name must have one child");
    assert!(messages.iter().any(|message| matches!(
        message.content,
        MessageContent::ToolResult {
            is_error: false,
            ..
        }
    )
        && message.content.text_content().as_deref()
            == Some("reader already exists; use send.")));
}

/// Background management uses ownership headers even when an unrelated transcript cannot be decoded.
#[tokio::test]
async fn background_management_only_reads_ownership_headers() {
    use std::io::Write;
    use store::StoreFactory;
    let test = background_test(|_| Route::Hang);
    let owner = test.create("owner").await;
    let factory = store::JsonlStoreFactory::new(
        test.root.path().join("sessions"),
        Arc::new(store::SystemClock),
    );
    let unrelated = factory
        .create(store::SessionCreateOptions {
            session_id: SessionId::try_from("unrelated").expect("id"),
            cwd: test.root.path().into(),
            parent_session_id: None,
        })
        .expect("unrelated");
    let path = unrelated.path().to_path_buf();
    drop(unrelated);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append")
        .write_all(&[0xff])
        .expect("opaque body");
    assert_eq!(
        invoke(&test, &owner, serde_json::json!({"action":"status"})).await,
        "No subagents."
    );
    assert_eq!(invoke(&test, &owner, serde_json::json!({"action":"spawn","name":"reader","message":"work"})).await, "Started reader.");
    test.kernel.close_session(&owner).await.expect("close");
}

/// Routes owner commands through the real background tool and scripts only child inference.
fn background_test(
    child: impl Fn(&str) -> Route + Send + Sync + 'static,
) -> TestKernel {
    TestKernel::new(move |r| {
        let message = r.messages.last().expect("input");
        let input = text(&message.content);
        if let Some(json) = input.strip_prefix("bg:") {
            Route::Events(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("background-call")
                        .expect("id"),
                    name: "background_subagent".into(),
                    arguments: serde_json::from_str(json).expect("args"),
                }),
                finished(StopReason::ToolUse),
            ])
        } else if matches!(message.content, MessageContent::ToolResult { .. })
            || input.starts_with("[subagent ")
        {
            Route::answer("ack")
        } else {
            child(&input)
        }
    })
}

/// Invokes one action and reads the persisted tool reply from its owner transcript.
async fn invoke(
    test: &TestKernel,
    owner: &SessionId,
    arguments: serde_json::Value,
) -> String {
    test.kernel
        .submit(request(
            owner,
            &format!("bg:{arguments}"),
            WhenBusy::FollowUp,
        ))
        .await
        .expect("owner action");
    test.kernel
        .session_transcript(owner)
        .expect("transcript")
        .iter()
        .rev()
        .find(|m| matches!(m.content, MessageContent::ToolResult { .. }))
        .and_then(|m| m.content.text_content())
        .expect("tool reply")
}

/// Background work returns immediately and later drives an idle owner's follow-up run.
#[tokio::test]
async fn spawn_reports_answer_to_idle_owner() {
    let gate = Arc::new(Notify::new());
    let release = Arc::clone(&gate);
    let test = background_test(move |_| {
        Route::Wait(
            Arc::clone(&gate),
            vec![
                ModelStreamEvent::TextDelta("A whale.".into()),
                finished(StopReason::EndTurn),
            ],
        )
    });
    let owner = test.create("owner").await;
    assert_eq!(invoke(&test, &owner, serde_json::json!({"action":"spawn","name":"reader","message":"Summarize."})).await, "Started reader.");
    test.wait_until(|| test.model.last_texts().iter().any(|t| t.starts_with("You are the subagent \"reader\". Answer the main agent's requests."))).await;
    release.notify_one();
    test.wait_until(|| {
        test.user_texts(&owner).contains(
            &"[subagent reader answered, no reply needed] A whale.".into(),
        )
    })
    .await;
    test.kernel.close_session(&owner).await.expect("close");
}

/// User-correctable requests produce plain replies without allocating invalid children.
#[tokio::test]
async fn user_errors_are_plain_replies() {
    let test = background_test(|_| Route::Hang);
    let owner = test.create("owner").await;
    for (args, expected) in [
        (serde_json::json!({"action":"send"}), "send needs a name."),
        (
            serde_json::json!({"action":"spawn", "name":"reader"}),
            "spawn needs a message.",
        ),
        (
            serde_json::json!({"action":"spawn", "name":"has space", "message":"x"}),
            "Invalid subagent name: use 1-64 letters, digits, '_' or '-'.",
        ),
        (
            serde_json::json!({"action":"spawn", "name":"a".repeat(65), "message":"x"}),
            "Invalid subagent name: use 1-64 letters, digits, '_' or '-'.",
        ),
        (
            serde_json::json!({"action":"send", "name":"ghost", "message":"x"}),
            "No subagent named ghost.",
        ),
    ] {
        assert_eq!(invoke(&test, &owner, args).await, expected);
    }
    assert!(test.owned_children(&owner).is_empty());
    assert_eq!(
        invoke(
            &test,
            &owner,
            serde_json::json!({"action":"spawn","name":"reader","message":"x"})
        )
        .await,
        "Started reader."
    );
    assert_eq!(
        invoke(
            &test,
            &owner,
            serde_json::json!({"action":"spawn","name":"reader","message":"x"})
        )
        .await,
        "reader already exists; use send."
    );
    assert!(
        test.kernel
            .session_transcript(&owner)
            .expect("transcript")
            .iter()
            .filter(|m| matches!(m.content, MessageContent::ToolResult { .. }))
            .all(|m| matches!(
                m.content,
                MessageContent::ToolResult {
                    is_error: false,
                    ..
                }
            ))
    );
    test.kernel.close_session(&owner).await.expect("close");
}

/// Stopping work clears queued input and suppresses a cancelled child's report.
#[tokio::test]
async fn stop_suppresses_report_and_status_lists_children() {
    let test = background_test(|_| Route::Hang);
    let owner = test.create("owner").await;
    assert_eq!(
        invoke(&test, &owner, serde_json::json!({"action":"status"})).await,
        "No subagents."
    );
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"reader","message":"x"}),
    )
    .await;
    let child = test
        .owned_children(&owner)
        .first()
        .expect("child")
        .session_id
        .clone();
    test.wait_until(|| {
        test.kernel
            .session_runtime(&child)
            .expect("runtime")
            .running
    })
    .await;
    assert_eq!(
        invoke(&test, &owner, serde_json::json!({"action":"status"})).await,
        "reader: working"
    );
    assert_eq!(
        invoke(
            &test,
            &owner,
            serde_json::json!({"action":"stop","name":"reader"})
        )
        .await,
        "Stopped reader."
    );
    test.wait_until(|| {
        !test
            .kernel
            .session_runtime(&child)
            .expect("runtime")
            .running
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !test
            .user_texts(&owner)
            .iter()
            .any(|t| t.starts_with("[subagent "))
    );
    test.kernel
        .close_session(&child)
        .await
        .expect("child close");
    assert_eq!(
        invoke(&test, &owner, serde_json::json!({"action":"status"})).await,
        "reader: idle"
    );
    assert_eq!(
        invoke(
            &test,
            &owner,
            serde_json::json!({"action":"stop","name":"reader"})
        )
        .await,
        "Stopped reader."
    );
    test.kernel.close_session(&owner).await.expect("close");
}

/// Sending to a closed child recreates its runtime without losing its registered name.
#[tokio::test]
async fn send_resumes_closed_child() {
    let test = background_test(|input| {
        if input == "follow up" {
            Route::answer("resumed answer")
        } else {
            Route::Hang
        }
    });
    let owner = test.create("owner").await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"reader","message":"x"}),
    )
    .await;
    let child = test
        .owned_children(&owner)
        .first()
        .expect("child")
        .session_id
        .clone();
    test.wait_until(|| {
        test.kernel
            .session_runtime(&child)
            .expect("runtime")
            .running
    })
    .await;
    test.kernel
        .close_session(&child)
        .await
        .expect("child close");
    assert_eq!(invoke(&test, &owner, serde_json::json!({"action":"send","name":"reader","message":"follow up","followUp":true})).await, "Sent to reader.");
    test.wait_until(|| {
        test.user_texts(&owner)
            .iter()
            .any(|t| t.ends_with("resumed answer"))
    })
    .await;
    test.kernel.close_session(&owner).await.expect("close");
}

/// Long child answers cannot bypass the ordinary tool text budget through user-message reports.
#[tokio::test]
async fn long_answer_report_is_truncated() {
    let test = background_test(|_| Route::answer(&"a".repeat(30 * 1024)));
    let owner = test.create("owner").await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"reader","message":"x"}),
    )
    .await;
    test.wait_until(|| {
        test.user_texts(&owner)
            .iter()
            .any(|t| t.starts_with("[subagent "))
    })
    .await;
    let report = test
        .user_texts(&owner)
        .into_iter()
        .find(|t| t.starts_with("[subagent "))
        .expect("report");
    assert!(report.len() <= tools::TOOL_TEXT_BYTES + 200);
    assert!(report.ends_with("[subagent answer truncated; open the subagent session for the full answer]"));
    test.kernel.close_session(&owner).await.expect("close");
}

/// Multiple reporters sharing one answering run publish its final assistant identity only once.
#[tokio::test]
async fn steered_messages_report_one_answer_once() {
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let test = background_test(move |input| {
        if input.starts_with("You are") {
            Route::Wait(Arc::clone(&gate), vec![finished(StopReason::EndTurn)])
        } else {
            Route::answer("final answer")
        }
    });
    let owner = test.create("owner").await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"reader","message":"x"}),
    )
    .await;
    let child = test
        .owned_children(&owner)
        .first()
        .expect("child")
        .session_id
        .clone();
    test.wait_until(|| {
        test.kernel
            .session_runtime(&child)
            .expect("runtime")
            .running
    })
    .await;
    for message in ["first", "second"] {
        invoke(&test, &owner, serde_json::json!({"action":"send","name":"reader","message":message})).await;
    }
    test.wait_until(|| {
        test.kernel
            .pending_messages(&child)
            .expect("queue")
            .steering
            .len()
            == 2
    })
    .await;
    release.notify_one();
    test.wait_until(|| {
        test.user_texts(&owner)
            .iter()
            .any(|t| t.starts_with("[subagent "))
    })
    .await;
    test.kernel
        .close_session(&owner)
        .await
        .expect("close reporters");
    test.kernel
        .resume_session(owner.clone(), test.root.path().into())
        .await
        .expect("resume");
    assert_eq!(
        test.user_texts(&owner)
            .iter()
            .filter(|t| t.starts_with("[subagent "))
            .count(),
        1
    );
    test.kernel.close_session(&owner).await.expect("close");
}

/// Owner close cancels delivery, waits for reporters, and never persists a late report.
#[tokio::test]
async fn closing_owner_with_reporter_in_delivery_does_not_hang() {
    let test = background_test(|_| Route::Hang);
    let owner = test.create("owner").await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"reader","message":"x"}),
    )
    .await;
    let child = test
        .owned_children(&owner)
        .first()
        .expect("child")
        .session_id
        .clone();
    test.wait_until(|| {
        test.kernel
            .session_runtime(&child)
            .expect("runtime")
            .running
    })
    .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        test.kernel.close_session(&owner),
    )
    .await
    .expect("no hang")
    .expect("close");
    test.kernel
        .resume_session(owner.clone(), test.root.path().into())
        .await
        .expect("resume");
    assert!(
        !test
            .user_texts(&owner)
            .iter()
            .any(|t| t.starts_with("[subagent "))
    );
    test.kernel.close_session(&owner).await.expect("close");
}
