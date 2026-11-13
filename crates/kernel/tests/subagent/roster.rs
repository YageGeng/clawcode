//! Completion notifications, `list_agents`, and the `<subagents>` roster note.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use kernel::ModelError;
use protocol::{MessageContent, ModelRequest, ModelStreamEvent, StopReason};
use tokio::sync::Notify;

use super::agents::{agents, call, child, held, last, prompt, spawn};
use super::fixture::{KernelPolicy, Route, TestKernel, finished, text};

const ROSTER_ONE: &str =
    "<subagents>\n  <agent name=\"/root/reader\" />\n</subagents>";
const ROSTER_EMPTY: &str = "<subagents>\n</subagents>";

/// Spawns `reader` with `message` and returns its Session id.
async fn spawn_reader(
    test: &TestKernel,
    owner: &protocol::SessionId,
) -> protocol::SessionId {
    spawn(
        test,
        owner,
        serde_json::json!([["s1", {"task_name": "reader", "message": "Read.", "fork_turns": "none"}]]),
    )
    .await;
    child(test, owner, "reader").session_id
}

/// Lists agents from `session` and parses the JSON reply.
async fn list(
    test: &TestKernel,
    session: &protocol::SessionId,
    args: serde_json::Value,
) -> serde_json::Value {
    let reply = call(
        test,
        session,
        serde_json::json!([["l", protocol::LIST_AGENTS_TOOL, args]]),
    )
    .await
    .remove(0);
    serde_json::from_str(&reply).expect("list JSON")
}

/// Counts hidden roster notes in one provider request and returns their texts.
fn roster_notes(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|m| {
            matches!(&m.content, MessageContent::Extension { extension }
            if extension.custom_type == protocol::SUBAGENT_ROSTER_TYPE)
        })
        .map(|m| text(&m.content))
        .collect()
}

/// A child's answer lands in an idle owner's history without waking it.
#[tokio::test]
async fn child_answer_is_mailed_to_idle_owner_without_waking() {
    let test = agents(KernelPolicy::default(), |_| Route::answer("A whale."));
    let owner = test.create("owner").await;
    spawn_reader(&test, &owner).await;
    let result = "Message Type: result\nTask name: /root\nSender: /root/reader\nPayload:\nA whale.";
    test.wait_until(|| test.agent_mails(&owner).iter().any(|m| m == result))
        .await;
    let requests = test.model.request_count();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(test.model.request_count(), requests);
    assert!(
        !test
            .kernel
            .session_runtime(&owner)
            .expect("runtime")
            .running
    );
}

/// A child's answer to a busy owner does not add a Turn to the owner's Run.
#[tokio::test]
async fn child_answer_to_busy_owner_does_not_extend_its_run() {
    let gate = Arc::new(Notify::new());
    let owner_gate = Arc::clone(&gate);
    let test = TestKernel::with_policy(
        move |request| {
            let input = last(request);
            if input.starts_with("call:") {
                super::fixture::calls(&[(
                    "s1",
                    protocol::SPAWN_AGENT_TOOL,
                    serde_json::json!({
                        "task_name": "reader", "message": "Read.", "fork_turns": "none"
                    }),
                )])
            } else if input.starts_with("Message Type: new_task") {
                Route::answer("A whale.")
            } else {
                held(&owner_gate, "owner answer")
            }
        },
        KernelPolicy::default(),
    );
    let owner = test.create("owner").await;
    let run = test.start(&owner, "call:spawn");
    test.wait_until(|| {
        test.agent_mails(&owner).is_empty()
            && test.owned_children(&owner).len() == 1
    })
    .await;
    let reader = child(&test, &owner, "reader").session_id;
    test.wait_until(|| {
        !test
            .kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
            && !test.agent_mails(&reader).is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    gate.notify_one();
    let result = run.await.expect("join").expect("run");
    assert_eq!(result.turns.len(), 2);
    assert!(
        test.agent_mails(&owner)
            .iter()
            .any(|m| m.ends_with("Payload:\nA whale."))
    );
}

/// A failed child reports its error; an interrupted child reports nothing.
#[tokio::test]
async fn failed_and_interrupted_children() {
    let test = agents(KernelPolicy::default(), |request| {
        if last(request).ends_with("Payload:\nfail") {
            Route::Fail(|| ModelError::AuthRequired {
                method_id: "x".to_owned(),
            })
        } else {
            Route::Hang
        }
    });
    let owner = test.create("owner").await;
    spawn(&test, &owner, serde_json::json!([["s1", {"task_name": "broken", "message": "fail", "fork_turns": "none"}]])).await;
    test.wait_until(|| {
        test.agent_mails(&owner)
            .iter()
            .any(|m| m.contains("Payload:\nAgent errored: "))
    })
    .await;
    let stuck = {
        spawn(&test, &owner, serde_json::json!([["s2", {"task_name": "stuck", "message": "wait", "fork_turns": "none"}]])).await;
        child(&test, &owner, "stuck").session_id
    };
    test.wait_until(|| {
        test.kernel
            .session_runtime(&stuck)
            .expect("runtime")
            .running
    })
    .await;
    test.kernel.cancel_session(&stuck).expect("cancel");
    test.wait_until(|| {
        !test
            .kernel
            .session_runtime(&stuck)
            .expect("runtime")
            .running
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !test
            .agent_mails(&owner)
            .iter()
            .any(|m| m.contains("Sender: /root/stuck"))
    );
    let listed = list(
        &test,
        &owner,
        serde_json::json!({ "path_prefix": "/root/stuck" }),
    )
    .await;
    let nickname = child(&test, &owner, "stuck")
        .ownership
        .expect("edge")
        .nickname;
    assert_eq!(
        listed,
        serde_json::json!({"agents": [{"agent_name": "/root/stuck", "agent_status": "interrupted", "nickname": nickname}]})
    );
    let broken = list(
        &test,
        &owner,
        serde_json::json!({ "path_prefix": "/root/broken" }),
    )
    .await;
    assert!(
        broken["agents"][0]["agent_status"]["errored"].is_string(),
        "{broken}"
    );
}

/// A user talking to a child directly still notifies the owner.
#[tokio::test]
async fn user_chat_in_child_also_notifies_owner() {
    let test = agents(KernelPolicy::default(), |_| Route::answer("first"));
    let owner = test.create("owner").await;
    let reader = spawn_reader(&test, &owner).await;
    test.wait_until(|| test.agent_mails(&owner).len() == 1)
        .await;
    prompt(&test, &reader, "hello").await;
    test.wait_until(|| test.agent_mails(&owner).len() == 2)
        .await;
    assert!(test.agent_mails(&owner)[1].ends_with("Payload:\nowner answer"));
}

/// Long answers are truncated with a hint; empty answers read `(no output)`.
#[tokio::test]
async fn long_answers_are_truncated_with_hint() {
    let test = agents(KernelPolicy::default(), |request| {
        if last(request).ends_with("Payload:\nempty") {
            Route::Events(vec![finished(StopReason::EndTurn)])
        } else if last(request).ends_with("Payload:\nlines") {
            Route::Events(vec![
                ModelStreamEvent::TextDelta("line\n".repeat(5_000)),
                finished(StopReason::EndTurn),
            ])
        } else {
            Route::Events(vec![
                ModelStreamEvent::TextDelta("x".repeat(30_000)),
                finished(StopReason::EndTurn),
            ])
        }
    });
    let owner = test.create("owner").await;
    spawn(&test, &owner, serde_json::json!([
        ["s1", {"task_name": "long", "message": "long", "fork_turns": "none"}],
        ["s2", {"task_name": "empty", "message": "empty", "fork_turns": "none"}],
        ["s3", {"task_name": "lines", "message": "lines", "fork_turns": "none"}]
    ])).await;
    test.wait_until(|| test.agent_mails(&owner).len() == 3)
        .await;
    let mails = test.agent_mails(&owner);
    let long = mails
        .iter()
        .find(|m| m.contains("Sender: /root/long"))
        .expect("long");
    let hint = "[answer truncated; open the agent session for the full answer]";
    assert!(long.ends_with(hint));
    // A single long line keeps its leading bytes instead of vanishing.
    assert!(long.contains(&"x".repeat(10_000)), "answer dropped");
    // An answer cut by length keeps its own hint; delivery does not cut it again.
    let lines = mails
        .iter()
        .find(|m| m.contains("Sender: /root/lines"))
        .expect("lines");
    assert!(lines.ends_with(hint), "lines answer lost its hint");
    assert!(!lines.contains("[message truncated]"));
    let empty = mails
        .iter()
        .find(|m| m.contains("Sender: /root/empty"))
        .expect("empty");
    assert!(empty.ends_with("Payload:\n(no output)"));
}

/// Statuses follow Codex's schema, and `path_prefix` filters by path.
#[tokio::test]
async fn list_agents_reports_codex_statuses() {
    let gate = Arc::new(Notify::new());
    let child_gate = Arc::clone(&gate);
    let test = agents(KernelPolicy::default(), move |_| {
        held(&child_gate, "A whale.")
    });
    let owner = test.create("owner").await;
    let reader = spawn_reader(&test, &owner).await;
    test.wait_until(|| {
        test.kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    })
    .await;
    // Only spawned agents carry the nickname the user sees; `/root` has none.
    let nickname = child(&test, &owner, "reader")
        .ownership
        .expect("edge")
        .nickname;
    assert_eq!(
        list(&test, &owner, serde_json::json!({})).await,
        serde_json::json!({"agents": [
            {"agent_name": "/root", "agent_status": "running"},
            {"agent_name": "/root/reader", "agent_status": "running", "nickname": nickname}
        ]})
    );
    gate.notify_one();
    test.wait_until(|| !test.agent_mails(&owner).is_empty())
        .await;
    assert_eq!(
        list(
            &test,
            &owner,
            serde_json::json!({"path_prefix": "/root/rea"})
        )
        .await,
        serde_json::json!({"agents": [
            {"agent_name": "/root/reader", "agent_status": {"completed": "A whale."}, "nickname": nickname}
        ]})
    );
}

/// The roster is stated once per change, emptied on deletion, and absent in children.
#[tokio::test]
async fn roster_note_is_stated_only_on_change() {
    let test = agents(KernelPolicy::default(), |_| Route::answer("done"));
    let owner = test.create("owner").await;
    let reader = spawn_reader(&test, &owner).await;
    test.wait_until(|| !test.agent_mails(&owner).is_empty())
        .await;
    prompt(&test, &owner, "again").await;
    prompt(&test, &owner, "and again").await;
    let requests = test.model.requests.lock().expect("requests").clone();
    let owner_requests = requests
        .iter()
        .filter(|r| !last(r).starts_with("Message Type: new_task"))
        .collect::<Vec<_>>();
    let latest = owner_requests.last().expect("owner request");
    assert_eq!(roster_notes(latest), vec![ROSTER_ONE.to_string()]);
    let child_request = requests
        .iter()
        .find(|r| last(r).starts_with("Message Type: new_task"))
        .expect("child request");
    assert!(roster_notes(child_request).is_empty());
    test.kernel
        .delete_session(&reader)
        .await
        .expect("delete child");
    prompt(&test, &owner, "after delete").await;
    let latest = test
        .model
        .requests
        .lock()
        .expect("requests")
        .last()
        .cloned()
        .expect("request");
    assert_eq!(
        roster_notes(&latest),
        vec![ROSTER_ONE.to_string(), ROSTER_EMPTY.to_string()]
    );
}

/// The roster lists at most eight children.
#[tokio::test]
async fn roster_note_is_bounded() {
    let test = agents(
        KernelPolicy {
            max_agents: NonZeroUsize::new(16),
            ..KernelPolicy::default()
        },
        |_| Route::answer("done"),
    );
    let owner = test.create("owner").await;
    let specs = (0..9)
        .map(|i| serde_json::json!([format!("s{i}"), {"task_name": format!("a{i}"), "message": "Go.", "fork_turns": "none"}]))
        .collect::<Vec<_>>();
    spawn(&test, &owner, serde_json::Value::Array(specs)).await;
    prompt(&test, &owner, "count").await;
    let latest = test
        .model
        .requests
        .lock()
        .expect("requests")
        .last()
        .cloned()
        .expect("request");
    let note = roster_notes(&latest).pop().expect("note");
    assert_eq!(note.matches("<agent name=").count(), 8, "{note}");
}

/// A reloaded owner rebuilds its roster from child headers.
#[tokio::test]
async fn registry_rebuilds_from_headers_after_restart() {
    let test = agents(KernelPolicy::default(), |_| Route::answer("done"));
    let owner = test.create("owner").await;
    spawn_reader(&test, &owner).await;
    test.wait_until(|| !test.agent_mails(&owner).is_empty())
        .await;
    test.kernel.close_session(&owner).await.expect("close");
    test.kernel
        .resume_session(owner.clone(), test.root.path().into())
        .await
        .expect("resume");
    // The nickname survives a restart because the roster reloads it from the child header.
    let nickname = child(&test, &owner, "reader")
        .ownership
        .expect("edge")
        .nickname;
    assert_eq!(
        list(
            &test,
            &owner,
            serde_json::json!({"path_prefix": "/root/reader"})
        )
        .await,
        serde_json::json!({"agents": [{"agent_name": "/root/reader", "agent_status": {"completed": null}, "nickname": nickname}]})
    );
}

/// Forking a child yields a standalone Session outside every roster.
#[tokio::test]
async fn forked_child_is_standalone() {
    let test = agents(KernelPolicy::default(), |_| Route::answer("done"));
    let owner = test.create("owner").await;
    let reader = spawn_reader(&test, &owner).await;
    test.wait_until(|| !test.agent_mails(&owner).is_empty())
        .await;
    let leaf = test
        .kernel
        .session_tree(&reader)
        .expect("tree")
        .leaf_id
        .expect("leaf");
    let fork = protocol::SessionId::try_from("reader-fork").expect("id");
    test.kernel
        .fork_session(&reader, leaf, fork.clone(), test.root.path().into())
        .await
        .expect("fork");
    let summary = test
        .kernel
        .list_sessions(Some(test.root.path()))
        .expect("list")
        .into_iter()
        .find(|s| s.session_id == fork)
        .expect("fork listed");
    assert!(summary.ownership.is_none());
    let listed = list(&test, &owner, serde_json::json!({})).await;
    assert_eq!(
        listed["agents"].as_array().expect("agents").len(),
        2,
        "{listed}"
    );
}

/// Closing an owner while its child still runs neither hangs nor reports.
#[tokio::test]
async fn closing_owner_with_pending_notification_does_not_hang() {
    let test = agents(KernelPolicy::default(), |_| Route::Hang);
    let owner = test.create("owner").await;
    let reader = spawn_reader(&test, &owner).await;
    test.wait_until(|| {
        test.kernel
            .session_runtime(&reader)
            .expect("runtime")
            .running
    })
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        test.kernel.close_session(&owner),
    )
    .await
    .expect("close in time")
    .expect("close");
}
