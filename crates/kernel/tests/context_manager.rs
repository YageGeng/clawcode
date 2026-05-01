use kernel::context::TurnContext;
use kernel::{ContextManager, SessionId, ThreadId};
use llm::{completion::Message, usage::Usage};

#[test]
fn turn_context_can_snapshot_and_fork_child() {
    let parent = TurnContext::new(SessionId::new(), ThreadId::new())
        .with_name("root-agent")
        .with_system_prompt("system")
        .with_cwd("/tmp/project")
        .with_current_date("2026-04-22")
        .with_timezone("Asia/Shanghai");

    let snapshot = parent.to_turn_context_item();
    assert_eq!(snapshot.session_id, parent.session_id);
    assert_eq!(snapshot.thread_id, parent.thread_id);
    assert_eq!(snapshot.system_prompt.as_deref(), Some("system"));

    let child = parent.fork_child("worker");
    assert_eq!(
        child.parent_agent_id.as_deref(),
        Some(parent.agent_id.as_str())
    );
    assert_eq!(child.session_id, parent.session_id);
    assert_ne!(child.thread_id, parent.thread_id);
    assert_eq!(child.system_prompt, parent.system_prompt);
}

#[test]
fn context_manager_tracks_active_turn_and_reference_snapshot() {
    let mut history = ContextManager::new();
    let turn_context =
        TurnContext::new(SessionId::new(), ThreadId::new()).with_system_prompt("system");

    history.begin_turn("hello".to_string(), Message::user("hello"));
    history.append_message(Message::assistant("world"));
    history.finalize_turn(Usage::new(), &turn_context);

    let prompt = history.prompt_messages(32);
    assert_eq!(prompt.len(), 2);
    assert_eq!(
        history.reference_context_item(),
        Some(turn_context.to_turn_context_item())
    );
}

#[test]
fn context_manager_reinjects_full_context_without_baseline() {
    let history = ContextManager::new();
    let turn_context = TurnContext::new(SessionId::new(), ThreadId::new())
        .with_system_prompt("system")
        .with_timezone("Asia/Shanghai");

    let initial_items = history.initial_context_items(&turn_context);
    assert_eq!(initial_items.len(), 2);
}

#[test]
fn context_manager_emits_diff_items_when_baseline_exists() {
    let mut history = ContextManager::new();
    let baseline = TurnContext::new(SessionId::new(), ThreadId::new()).with_system_prompt("system");
    history.set_reference_context_item(Some(baseline.to_turn_context_item()));

    let current = baseline.clone().with_timezone("Asia/Shanghai");
    let diff_items = history.settings_diff_items(&current);
    assert_eq!(diff_items.len(), 1);
}
