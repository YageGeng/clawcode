//! `send_message`, `followup_task`, `wait_agent`, and `interrupt_agent`.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use protocol::{
    MailKind, ModelRequest, ModelStreamEvent, QueueKind, StopReason,
};
use tokio::sync::Notify;

use super::agents::{agents, call, child, held, last, spawn};
use super::fixture::{
    KernelPolicy, Route, TestKernel, finished, tool_failed, tool_result,
};

/// Spawns one child without inherited history and returns its Session id.
async fn spawn_named(
    test: &TestKernel,
    owner: &protocol::SessionId,
    name: &str,
    message: &str,
) -> protocol::SessionId {
    spawn(
        test,
        owner,
        serde_json::json!([[format!("spawn-{name}"), {"task_name": name, "message": message, "fork_turns": "none"}]]),
    )
    .await;
    child(test, owner, name).session_id
}

/// Waits until a Session has no active Run.
async fn idle(test: &TestKernel, session: &protocol::SessionId) {
    test.wait_until(|| {
        !test
            .kernel
            .session_runtime(session)
            .expect("runtime")
            .running
    })
    .await;
}

/// Child answers: `call:` payloads run tools, `hold` waits on `gate`, others answer at once.
fn scripted(
    gate: Arc<Notify>,
) -> impl Fn(&ModelRequest) -> Route + Send + Sync + 'static {
    move |request| {
        if last(request).ends_with("Payload:\nhold") {
            held(&gate, "held answer")
        } else {
            Route::answer("done")
        }
    }
}

/// Children reach siblings and `/root`, and read the owner's roster.
#[tokio::test]
async fn send_message_reaches_sibling_and_root() {
    let test =
        agents(KernelPolicy::default(), scripted(Arc::new(Notify::new())));
    let owner = test.create("owner").await;
    let b = spawn_named(&test, &owner, "b", "wait").await;
    idle(&test, &b).await;
    let script = serde_json::json!([
        ["m1", protocol::SEND_MESSAGE_TOOL, {"target": "b", "message": "hi b"}],
        ["m2", protocol::SEND_MESSAGE_TOOL, {"target": "/root", "message": "hi root"}],
        ["m3", protocol::LIST_AGENTS_TOOL, {}]
    ]);
    let a = spawn_named(&test, &owner, "a", &format!("call:{script}")).await;
    test.wait_until(|| test.agent_mails(&b).len() == 2).await;
    idle(&test, &a).await;
    assert_eq!(tool_result(&test, &a, "m1").0, "Message sent to /root/b.");
    assert_eq!(tool_result(&test, &a, "m2").0, "Message sent to /root.");
    let listed: serde_json::Value =
        serde_json::from_str(&tool_result(&test, &a, "m3").0).expect("list");
    let names = listed["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .map(|agent| agent["agent_name"].as_str().expect("name").to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["/root", "/root/a", "/root/b"]);
    assert_eq!(
        test.agent_mails(&b)[1],
        "Message Type: message\nTask name: /root/b\nSender: /root/a\nPayload:\nhi b"
    );
    assert!(
        test.agent_mails(&owner)
            .iter()
            .any(|m| m.ends_with("Payload:\nhi root"))
    );
    assert!(!test.kernel.session_runtime(&b).expect("runtime").running);
}

/// A follow-up wakes an idle child, reaches a busy one, and respects the limit.
#[tokio::test]
async fn followup_task_wakes_idle_child_and_extends_busy_child() {
    let gate = Arc::new(Notify::new());
    let test = agents(
        KernelPolicy {
            max_agents: NonZeroUsize::new(1),
            ..KernelPolicy::default()
        },
        scripted(Arc::clone(&gate)),
    );
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "first").await;
    idle(&test, &reader).await;
    let sent = call(&test, &owner, serde_json::json!([["f1", protocol::FOLLOWUP_TASK_TOOL, {"target": "reader", "message": "hold"}]])).await;
    assert_eq!(sent, vec!["Task sent to /root/reader."]);
    test.wait_until(|| {
        test.kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    })
    .await;
    let busy = call(&test, &owner, serde_json::json!([["f2", protocol::FOLLOWUP_TASK_TOOL, {"target": "reader", "message": "more"}]])).await;
    assert_eq!(busy, vec!["Task sent to /root/reader."]);
    gate.notify_one();
    test.wait_until(|| {
        test.model
            .last_texts()
            .iter()
            .any(|t| t == "Message Type: new_task\nTask name: /root/reader\nSender: /root\nPayload:\nmore")
    })
    .await;
    idle(&test, &reader).await;
    let other = spawn_named(&test, &owner, "other", "hold").await;
    test.wait_until(|| {
        test.kernel
            .session_runtime(&other)
            .expect("runtime")
            .running
    })
    .await;
    let limited = call(&test, &owner, serde_json::json!([["f3", protocol::FOLLOWUP_TASK_TOOL, {"target": "reader", "message": "again"}]])).await;
    assert_eq!(
        limited,
        vec![
            "agent limit reached: 1 agents are running; wait for one to finish."
        ]
    );
    gate.notify_waiters();
}

/// Waiting ends on mail, on user input, on pending mail at once, or on timeout.
#[tokio::test(start_paused = true)]
async fn wait_agent_returns_on_mail_input_and_timeout() {
    let gate = Arc::new(Notify::new());
    let test = agents(KernelPolicy::default(), scripted(Arc::clone(&gate)));
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "hold").await;
    test.wait_until(|| {
        test.kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    })
    .await;
    let wait = serde_json::json!([["w1", protocol::WAIT_AGENT_TOOL, {"timeout_ms": 600000}]]);
    let run = test.start(&owner, &format!("call:{wait}"));
    test.wait_until(|| {
        test.model
            .last_texts()
            .iter()
            .filter(|t| t.starts_with("call:"))
            .count()
            == 2
    })
    .await;
    gate.notify_one();
    run.await.expect("join").expect("run");
    assert_eq!(tool_result(&test, &owner, "w1").0, "Wait completed.");

    let wait = serde_json::json!([["w2", protocol::WAIT_AGENT_TOOL, {"timeout_ms": 600000}]]);
    let run = test.start(&owner, &format!("call:{wait}"));
    test.wait_until(|| {
        test.kernel
            .session_runtime(&owner)
            .expect("runtime")
            .running
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    test.kernel
        .queue_message(&owner, QueueKind::Steering, "user says hi")
        .await
        .expect("steer");
    run.await.expect("join").expect("run");
    assert_eq!(
        tool_result(&test, &owner, "w2").0,
        "Wait interrupted by new input."
    );

    let timed = call(&test, &owner, serde_json::json!([["w3", protocol::WAIT_AGENT_TOOL, {"timeout_ms": 10000}]])).await;
    assert_eq!(timed, vec!["Wait timed out."]);
    let clamped = call(&test, &owner, serde_json::json!([["w4", protocol::WAIT_AGENT_TOOL, {"timeout_ms": 5}]])).await;
    assert_eq!(
        clamped,
        vec![
            "Wait timed out.\n\nRequested timeout of 5ms was clamped to the minimum of 10000ms."
        ]
    );
    let too_long = call(&test, &owner, serde_json::json!([["w5", protocol::WAIT_AGENT_TOOL, {"timeout_ms": 4000000}]])).await;
    assert_eq!(too_long, vec!["timeout_ms must be at most 3600000"]);
}

/// Mail already queued when `wait_agent` starts ends the wait at once.
#[tokio::test]
async fn wait_agent_returns_at_once_for_pending_mail() {
    let gate = Arc::new(Notify::new());
    let owner_gate = Arc::clone(&gate);
    let test = TestKernel::new(move |request| {
        if last(request) == "go" {
            Route::Wait(
                Arc::clone(&owner_gate),
                vec![
                    ModelStreamEvent::ToolCall(
                        protocol::ToolCall::builder()
                            .tool_call_id(
                                protocol::ToolCallId::try_from("w")
                                    .expect("id"),
                            )
                            .name(protocol::WAIT_AGENT_TOOL.into())
                            .arguments(
                                serde_json::json!({"timeout_ms": 3600000}),
                            )
                            .build(),
                    ),
                    finished(StopReason::ToolUse),
                ],
            )
        } else {
            Route::answer("done")
        }
    });
    let owner = test.create("owner").await;
    let run = test.start(&owner, "go");
    test.wait_until(|| test.model.request_count() == 1).await;
    test.mail(&owner, "/root/x", MailKind::Message, "early")
        .await;
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("wait returned")
        .expect("join")
        .expect("run");
    assert_eq!(tool_result(&test, &owner, "w").0, "Wait completed.");
}

/// Interrupting stops a child's Run; `/root` is never a follow-up or interrupt target.
#[tokio::test]
async fn interrupt_agent_cancels_child_and_rejects_root() {
    let gate = Arc::new(Notify::new());
    let test = agents(KernelPolicy::default(), scripted(Arc::clone(&gate)));
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "hold").await;
    test.wait_until(|| {
        test.kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    })
    .await;
    let interrupted = call(&test, &owner, serde_json::json!([["i1", protocol::INTERRUPT_AGENT_TOOL, {"target": "reader"}]])).await;
    assert_eq!(interrupted, vec![r#"{"previous_status":"running"}"#]);
    idle(&test, &reader).await;
    let listed = call(&test, &owner, serde_json::json!([["l", protocol::LIST_AGENTS_TOOL, {"path_prefix": "/root/reader"}]])).await;
    assert_eq!(
        listed,
        vec![
            r#"{"agents":[{"agent_name":"/root/reader","agent_status":"interrupted"}]}"#
        ]
    );
    let root = call(&test, &owner, serde_json::json!([
        ["i2", protocol::INTERRUPT_AGENT_TOOL, {"target": "/root"}],
        ["f1", protocol::FOLLOWUP_TASK_TOOL, {"target": "/root", "message": "x"}]
    ])).await;
    assert_eq!(
        root,
        vec!["root is not a spawned agent", "root is not a spawned agent"]
    );
}

/// Messages to oneself and empty messages are correctable replies.
#[tokio::test]
async fn self_targets_and_empty_messages_are_rejected() {
    let test =
        agents(KernelPolicy::default(), scripted(Arc::new(Notify::new())));
    let owner = test.create("owner").await;
    let root = call(&test, &owner, serde_json::json!([
        ["s1", protocol::SEND_MESSAGE_TOOL, {"target": "/root", "message": "me"}],
        ["s2", protocol::SEND_MESSAGE_TOOL, {"target": "x", "message": "  "}]
    ])).await;
    assert_eq!(
        root,
        vec![
            "an agent cannot message itself",
            "message must not be empty"
        ]
    );
    let script = serde_json::json!([["c1", protocol::SEND_MESSAGE_TOOL, {"target": "solo", "message": "me"}]]);
    let solo =
        spawn_named(&test, &owner, "solo", &format!("call:{script}")).await;
    idle(&test, &solo).await;
    assert_eq!(
        tool_result(&test, &solo, "c1").0,
        "an agent cannot message itself"
    );
}

/// Children see exactly the three mailbox tools among the agent tools.
#[tokio::test]
async fn children_see_only_mailbox_tools() {
    let test =
        agents(KernelPolicy::default(), scripted(Arc::new(Notify::new())));
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "go").await;
    idle(&test, &reader).await;
    let requests = test.model.requests.lock().expect("requests").clone();
    let child_request = requests
        .iter()
        .find(|r| last(r).starts_with("Message Type: new_task"))
        .expect("child request");
    let agent_tools = [
        protocol::SPAWN_AGENT_TOOL,
        protocol::SEND_MESSAGE_TOOL,
        protocol::FOLLOWUP_TASK_TOOL,
        protocol::WAIT_AGENT_TOOL,
        protocol::LIST_AGENTS_TOOL,
        protocol::INTERRUPT_AGENT_TOOL,
    ];
    let mut seen = child_request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .filter(|name| agent_tools.contains(name))
        .collect::<Vec<_>>();
    seen.sort_unstable();
    assert_eq!(seen, vec!["list_agents", "send_message", "wait_agent"]);
}

/// Unknown and deleted targets read as missing paths.
#[tokio::test]
async fn unknown_and_deleted_targets() {
    let test =
        agents(KernelPolicy::default(), scripted(Arc::new(Notify::new())));
    let owner = test.create("owner").await;
    let ghost = call(&test, &owner, serde_json::json!([["g", protocol::SEND_MESSAGE_TOOL, {"target": "ghost", "message": "hi"}]])).await;
    assert_eq!(ghost, vec!["live agent path `/root/ghost` not found"]);
    let reader = spawn_named(&test, &owner, "reader", "go").await;
    idle(&test, &reader).await;
    test.kernel.delete_session(&reader).await.expect("delete");
    let gone = call(&test, &owner, serde_json::json!([["d", protocol::SEND_MESSAGE_TOOL, {"target": "reader", "message": "hi"}]])).await;
    assert_eq!(gone, vec!["live agent path `/root/reader` not found"]);
}

/// Mail to an unloaded child loads it and writes the mail without a Run.
#[tokio::test]
async fn mail_to_unloaded_child_resumes_without_running() {
    let test =
        agents(KernelPolicy::default(), scripted(Arc::new(Notify::new())));
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "go").await;
    idle(&test, &reader).await;
    test.kernel.close_session(&owner).await.expect("close");
    test.kernel
        .resume_session(owner.clone(), test.root.path().into())
        .await
        .expect("resume");
    let child_requests = |test: &TestKernel| {
        test.model
            .last_texts()
            .iter()
            .filter(|t| t.contains("Task name: /root/reader"))
            .count()
    };
    let before = child_requests(&test);
    let sent = call(&test, &owner, serde_json::json!([["m", protocol::SEND_MESSAGE_TOOL, {"target": "reader", "message": "later"}]])).await;
    assert_eq!(sent, vec!["Message sent to /root/reader."]);
    assert!(
        test.agent_mails(&reader)
            .last()
            .expect("mail")
            .ends_with("Payload:\nlater")
    );
    assert_eq!(child_requests(&test), before);
    assert!(
        !test
            .kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    );
}

/// A send to a settling child and that child's result to the owner both complete.
#[tokio::test]
async fn crossing_deliveries_do_not_deadlock() {
    let gate = Arc::new(Notify::new());
    let test = agents(KernelPolicy::default(), scripted(Arc::clone(&gate)));
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "hold").await;
    test.wait_until(|| {
        test.kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    })
    .await;
    let sends = (0..5)
        .map(|i| serde_json::json!([format!("x{i}"), protocol::SEND_MESSAGE_TOOL, {"target": "reader", "message": format!("ping {i}")}]))
        .collect::<Vec<_>>();
    let run = test
        .start(&owner, &format!("call:{}", serde_json::Value::Array(sends)));
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("owner finished")
        .expect("join")
        .expect("run");
    tokio::time::timeout(Duration::from_secs(10), idle(&test, &reader))
        .await
        .expect("child settled");
    test.wait_until(|| {
        test.agent_mails(&owner)
            .iter()
            .any(|m| m.contains("Sender: /root/reader"))
    })
    .await;
}

/// Refusals stay correctable text but are marked as errors, as Codex marks
/// `RespondToModel` failures; accepted calls are not.
#[tokio::test]
async fn refusals_are_marked_as_errors() {
    let test =
        agents(KernelPolicy::default(), scripted(Arc::new(Notify::new())));
    let owner = test.create("owner").await;
    let reader = spawn_named(&test, &owner, "reader", "Go.").await;
    idle(&test, &reader).await;
    call(&test, &owner, serde_json::json!([
        ["bad-name", protocol::SPAWN_AGENT_TOOL, {"task_name": "Reader", "message": "Go."}],
        ["duplicate", protocol::SPAWN_AGENT_TOOL, {"task_name": "reader", "message": "Go."}],
        ["ghost", protocol::SEND_MESSAGE_TOOL, {"target": "ghost", "message": "hi"}],
        ["root-task", protocol::FOLLOWUP_TASK_TOOL, {"target": "/root", "message": "x"}],
        ["root-stop", protocol::INTERRUPT_AGENT_TOOL, {"target": "/root"}],
        ["long-wait", protocol::WAIT_AGENT_TOOL, {"timeout_ms": 4_000_000}],
        ["listed", protocol::LIST_AGENTS_TOOL, {}],
        ["sent", protocol::SEND_MESSAGE_TOOL, {"target": "reader", "message": "hi"}],
        ["stopped", protocol::INTERRUPT_AGENT_TOOL, {"target": "reader"}]
    ])).await;
    for (id, refused) in [
        ("spawn-reader", false),
        ("bad-name", true),
        ("duplicate", true),
        ("ghost", true),
        ("root-task", true),
        ("root-stop", true),
        ("long-wait", true),
        ("listed", false),
        ("sent", false),
        ("stopped", false),
    ] {
        assert_eq!(tool_failed(&test, &owner, id), refused, "{id}");
    }
}
