//! `spawn_agent`: naming, limits, inherited history, overrides, and child tools.

use std::num::NonZeroUsize;
use std::sync::Arc;

use protocol::{
    MessageContent, ModelRequest, ThinkingLevel, ToolResultDetails,
};
use tokio::sync::Notify;

use super::agents::{agents, child, held, last, prompt, spawn};
use super::fixture::{KernelPolicy, TestKernel, text, tool_result};

/// Children wait on `gate` so they stay running until the test releases them.
fn spawning(gate: Arc<Notify>, policy: KernelPolicy) -> TestKernel {
    agents(policy, move |_| held(&gate, "child answer"))
}

/// Spawning returns the canonical task name at once and starts the child on a new task.
#[tokio::test]
async fn spawn_returns_immediately_and_starts_child() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    let results = spawn(
        &test,
        &owner,
        serde_json::json!([["call-1", {"task_name": "reader", "message": "Summarize."}]]),
    )
    .await;
    assert_eq!(results, vec![r#"{"task_name":"/root/reader"}"#]);
    let summary = child(&test, &owner, "reader");
    let edge = summary.ownership.clone().expect("edge");
    assert_eq!(edge.session_id, owner);
    assert!(!edge.nickname.is_empty());
    assert_eq!(summary.name.as_deref(), Some(edge.nickname.as_str()));
    let (_, details) = tool_result(&test, &owner, "call-1");
    assert_eq!(
        details,
        Some(ToolResultDetails::Subagent {
            session_id: summary.session_id.clone()
        })
    );
    test.wait_until(|| !test.agent_mails(&summary.session_id).is_empty())
        .await;
    let task = test.agent_mails(&summary.session_id).remove(0);
    assert!(
        task.contains("Task name: /root/reader\nSender: /root"),
        "{task}"
    );
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// `fork_turns` copies the owner's conversation, never its tool traffic.
#[tokio::test]
async fn fork_all_copies_filtered_history_without_usage() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    prompt(&test, &owner, "hello").await;
    let results = spawn(
        &test,
        &owner,
        serde_json::json!([
            ["call-all", {"task_name": "all", "message": "Go."}],
            ["call-none", {"task_name": "none", "message": "Go.", "fork_turns": "none"}],
            ["call-three", {"task_name": "three", "message": "Go.", "fork_turns": "3"}],
            ["call-bad", {"task_name": "bad", "message": "Go.", "fork_turns": "some"}]
        ]),
    )
    .await;
    assert_eq!(results[3], "fork_turns must be `none` or `all`");
    for name in ["all", "three"] {
        let child = child(&test, &owner, name).session_id;
        let transcript =
            test.kernel.session_transcript(&child).expect("transcript");
        let inherited = transcript
            .iter()
            .take_while(|m| {
                !matches!(m.content, MessageContent::Extension { .. })
            })
            .collect::<Vec<_>>();
        let texts = inherited
            .iter()
            .map(|m| text(&m.content))
            .collect::<Vec<_>>();
        assert_eq!(texts.first().map(String::as_str), Some("hello"));
        assert_eq!(texts.get(1).map(String::as_str), Some("owner answer"));
        assert!(texts.iter().any(|t| t.starts_with("call:")));
        assert!(
            !inherited.iter().any(|m| matches!(
                m.content,
                MessageContent::ToolResult { .. }
            ))
        );
        assert!(inherited.iter().all(|m| match &m.content {
            MessageContent::Assistant {
                blocks,
                model_message,
                ..
            } => {
                model_message.is_none()
                    && blocks.iter().all(|b| {
                        matches!(b, protocol::ContentBlock::Text { .. })
                    })
            }
            _ => true,
        }));
    }
    let none = child(&test, &owner, "none").session_id;
    assert!(matches!(
        test.kernel
            .session_transcript(&none)
            .expect("transcript")
            .first()
            .expect("first")
            .content,
        MessageContent::Extension { .. }
    ));
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Model and reasoning overrides apply to the child; unknown values are plain replies.
#[tokio::test]
async fn spawn_applies_model_and_reasoning_overrides() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    let results = spawn(
        &test,
        &owner,
        serde_json::json!([
            ["call-high", {"task_name": "high", "message": "Go.", "reasoning_effort": "high"}],
            ["call-off", {"task_name": "off", "message": "Go.", "reasoning_effort": "none", "model": "fixture/primary"}],
            ["call-model", {"task_name": "model", "message": "Go.", "model": "nope/x"}],
            ["call-effort", {"task_name": "effort", "message": "Go.", "reasoning_effort": "max"}]
        ]),
    )
    .await;
    let settings = |name| {
        test.kernel
            .session_settings(&child(&test, &owner, name).session_id)
            .expect("settings")
    };
    assert_eq!(settings("high").thinking_level, ThinkingLevel::High);
    assert_eq!(settings("off").thinking_level, ThinkingLevel::Off);
    assert_eq!(settings("off").model.model_id, "primary");
    assert!(
        results[2].starts_with("unknown model 'nope/x'"),
        "{}",
        results[2]
    );
    assert!(results[3].contains("reasoning_effort"), "{}", results[3]);
    assert_eq!(test.owned_children(&owner).len(), 2);
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Bad names and duplicates are correctable replies that create nothing.
#[tokio::test]
async fn invalid_and_duplicate_names_are_plain_replies() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    let results = spawn(
        &test,
        &owner,
        serde_json::json!([
            ["c1", {"task_name": "Reader", "message": "Go."}],
            ["c2", {"task_name": "代码审查", "message": "Go."}],
            ["c3", {"task_name": "root", "message": "Go."}],
            ["c4", {"task_name": "a/b", "message": "Go."}],
            ["c5", {"task_name": "reader", "message": "  "}]
        ]),
    )
    .await;
    let charset =
        "agent_name must use only lowercase letters, digits, and underscores";
    assert_eq!(results[0], charset);
    assert_eq!(results[1], charset);
    assert_eq!(results[2], "agent_name `root` is reserved");
    assert_eq!(results[3], "agent_name must not contain `/`");
    assert_eq!(results[4], "message must not be empty");
    assert!(test.owned_children(&owner).is_empty());
    spawn(
        &test,
        &owner,
        serde_json::json!([["d1", {"task_name": "reader", "message": "Go."}]]),
    )
    .await;
    let again = spawn(
        &test,
        &owner,
        serde_json::json!([["d2", {"task_name": "reader", "message": "Go."}]]),
    )
    .await;
    assert_eq!(again, vec!["reader already exists; use followup_task."]);
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Two same-name spawns in one batch create exactly one child.
#[tokio::test]
async fn concurrent_same_name_spawns_create_one_child() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    let mut results = spawn(
        &test,
        &owner,
        serde_json::json!([
            ["a", {"task_name": "reader", "message": "Go."}],
            ["b", {"task_name": "reader", "message": "Go."}]
        ]),
    )
    .await;
    results.sort();
    assert_eq!(
        results,
        vec![
            "reader already exists; use followup_task.".to_string(),
            r#"{"task_name":"/root/reader"}"#.to_string(),
        ]
    );
    assert_eq!(test.owned_children(&owner).len(), 1);
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// The running-child limit holds across batches and inside one batch.
#[tokio::test]
async fn spawn_respects_max_agents() {
    let gate = Arc::new(Notify::new());
    let test = spawning(
        Arc::clone(&gate),
        KernelPolicy {
            max_agents: NonZeroUsize::new(1),
            ..KernelPolicy::default()
        },
    );
    let owner = test.create("owner").await;
    let limit =
        "agent limit reached: 1 agents are running; wait for one to finish.";
    let batch = spawn(
        &test,
        &owner,
        serde_json::json!([
            ["a", {"task_name": "one", "message": "Go."}],
            ["b", {"task_name": "two", "message": "Go."}]
        ]),
    )
    .await;
    assert_eq!(
        batch.iter().filter(|r| r.as_str() == limit).count(),
        1,
        "{batch:?}"
    );
    let later = spawn(
        &test,
        &owner,
        serde_json::json!([["c", {"task_name": "three", "message": "Go."}]]),
    )
    .await;
    assert_eq!(later, vec![limit]);
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Closing a child that has not finished its first Run frees its slot.
#[tokio::test]
async fn closing_an_unstarted_child_releases_its_slot() {
    let gate = Arc::new(Notify::new());
    let test = spawning(
        Arc::clone(&gate),
        KernelPolicy {
            max_agents: NonZeroUsize::new(1),
            ..KernelPolicy::default()
        },
    );
    let owner = test.create("owner").await;
    spawn(
        &test,
        &owner,
        serde_json::json!([["a", {"task_name": "one", "message": "Go."}]]),
    )
    .await;
    let first = child(&test, &owner, "one").session_id;
    test.kernel
        .close_session(&first)
        .await
        .expect("close child");
    let next = spawn(
        &test,
        &owner,
        serde_json::json!([["b", {"task_name": "two", "message": "Go."}]]),
    )
    .await;
    assert_eq!(next, vec![r#"{"task_name":"/root/two"}"#]);
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Children never see the root-only agent tools.
#[tokio::test]
async fn child_lacks_root_only_tools() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    spawn(
        &test,
        &owner,
        serde_json::json!([["a", {"task_name": "reader", "message": "Go."}]]),
    )
    .await;
    test.wait_until(|| {
        test.model
            .requests
            .lock()
            .expect("requests")
            .iter()
            .any(|r| last(r).starts_with("Message Type: new_task"))
    })
    .await;
    let requests = test.model.requests.lock().expect("requests").clone();
    let names = |request: &ModelRequest| {
        request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    };
    let owner_tools = names(requests.first().expect("owner request"));
    assert!(
        owner_tools
            .iter()
            .any(|name| name == protocol::SPAWN_AGENT_TOOL)
    );
    let child_request = requests
        .iter()
        .find(|r| last(r).starts_with("Message Type: new_task"))
        .expect("child request");
    // Only spawning is reserved for `/root`; children keep Codex's other agent tools.
    for (name, child_has) in [
        (protocol::SPAWN_AGENT_TOOL, false),
        (protocol::FOLLOWUP_TASK_TOOL, true),
        (protocol::INTERRUPT_AGENT_TOOL, true),
    ] {
        assert_eq!(
            names(child_request).iter().any(|tool| tool == name),
            child_has,
            "{name}"
        );
    }
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Finds the first inter-agent mail in `messages`.
fn first_mail(messages: &[protocol::AgentMessage]) -> usize {
    messages
        .iter()
        .position(|m| {
            matches!(&m.content, MessageContent::Extension { extension }
                if extension.custom_type == protocol::AGENT_MESSAGE_TYPE)
        })
        .expect("mail")
}

/// A child's history ends with its hidden role note right before its first
/// task, after any inherited owner history, so neither prefix ever changes.
#[tokio::test]
async fn child_starts_with_role_note_after_inherited_history() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    prompt(&test, &owner, "hello").await;
    spawn(
        &test,
        &owner,
        serde_json::json!([
            ["call-all", {"task_name": "reader", "message": "Go."}],
            ["call-none", {"task_name": "solo", "message": "Go.", "fork_turns": "none"}]
        ]),
    )
    .await;
    for (name, inherits) in [("reader", true), ("solo", false)] {
        let summary = child(&test, &owner, name);
        let nickname = summary.ownership.expect("edge").nickname;
        let child = summary.session_id;
        test.wait_until(|| !test.agent_mails(&child).is_empty())
            .await;
        let transcript =
            test.kernel.session_transcript(&child).expect("transcript");
        let task = first_mail(&transcript);
        let note = &transcript[task.checked_sub(1).expect("note before task")];
        let MessageContent::Extension { extension } = &note.content else {
            panic!("{name}: {:?}", note.content);
        };
        assert_eq!(extension.custom_type, protocol::SUBAGENT_ROLE_TYPE);
        assert!(!extension.display && extension.include_in_context);
        let role = text(&note.content);
        assert!(
            role.starts_with(&format!("You are `/root/{name}`")),
            "{role}"
        );
        // The child learns the nickname the user sees, so it answers to it.
        assert!(
            role.contains(&format!("shown to the user as {nickname}")),
            "{role}"
        );
        // The note offers the Codex tools a child holds and withholds only spawning.
        assert!(
            role.contains("`followup_task`")
                && role.contains("`interrupt_agent`")
                && role.ends_with("but you cannot spawn agents."),
            "{role}"
        );
        assert_eq!(task > 1, inherits, "{name}");
    }
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}

/// Every agent reads the same System Prompt, which states the team's paths,
/// and a child reads its role note right before its task.
#[tokio::test]
async fn owner_and_child_share_one_system_prompt() {
    let gate = Arc::new(Notify::new());
    let test = spawning(Arc::clone(&gate), KernelPolicy::default());
    let owner = test.create("owner").await;
    spawn(
        &test,
        &owner,
        serde_json::json!([["a", {"task_name": "reader", "message": "Go."}]]),
    )
    .await;
    let is_child =
        |r: &ModelRequest| last(r).starts_with("Message Type: new_task");
    test.wait_until(|| {
        test.model
            .requests
            .lock()
            .expect("requests")
            .iter()
            .any(is_child)
    })
    .await;
    let requests = test.model.requests.lock().expect("requests").clone();
    let system = |request: &ModelRequest| match &request.messages[0].content {
        MessageContent::System { blocks } => blocks
            .iter()
            .filter_map(protocol::ContentBlock::text)
            .collect::<String>(),
        other => panic!("{other:?}"),
    };
    let owner_system = system(requests.first().expect("owner request"));
    assert!(
        owner_system.contains("`/root` is the primary agent"),
        "{owner_system}"
    );
    let child_request = requests.iter().find(|r| is_child(r)).expect("child");
    assert_eq!(system(child_request), owner_system);
    let task = first_mail(&child_request.messages);
    assert!(
        text(&child_request.messages[task - 1].content)
            .starts_with("You are `/root/reader`")
    );
    gate.notify_waiters();
    test.kernel.close_session(&owner).await.expect("close");
}
