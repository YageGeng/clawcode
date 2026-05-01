use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use kernel::{
    CollaborationSession, Result, ThreadHandle, ThreadRuntime, TurnContext,
    events::RecordingEventSink,
    model::{AgentModel, ModelRequest, ModelResponse},
    session::{InMemorySessionStore, SessionId, ThreadId},
};
use llm::{completion::Message, usage::Usage};
use store::{JsonlSessionStore, SessionEvent, load_session_events};
use tokio::sync::Notify;
use tools::{
    AgentRuntimeContext, AgentStatus, CloseAgentRequest, ListAgentsRequest, MailboxEventKind,
    SendAgentInputRequest, SpawnAgentRequest, WaitAgentRequest,
};

/// Model double that responds with the latest user-text payload it receives.
#[derive(Debug, Clone, Default)]
struct EchoModel;

#[async_trait(?Send)]
impl AgentModel for EchoModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        let text = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User { content } => content.iter().find_map(|part| match part {
                    llm::completion::message::UserContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_else(|| "empty".to_string());

        Ok(ModelResponse::text(
            text,
            Usage {
                input_tokens: 3,
                output_tokens: 3,
                total_tokens: 6,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        ))
    }
}

/// Model double that blocks the first completion until the test releases it.
#[derive(Debug, Clone)]
struct GatedEchoModel {
    started: Arc<Notify>,
    release: Arc<Notify>,
    first_call: Arc<AtomicBool>,
}

impl GatedEchoModel {
    /// Builds a controllable model used to exercise close-vs-finish race conditions.
    fn new(started: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self {
            started,
            release,
            first_call: Arc::new(AtomicBool::new(true)),
        }
    }
}

#[async_trait(?Send)]
impl AgentModel for GatedEchoModel {
    /// Echoes the latest user input after blocking the first model call until released.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        if self.first_call.swap(false, Ordering::SeqCst) {
            self.started.notify_waiters();
            self.release.notified().await;
        }

        let text = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User { content } => content.iter().find_map(|part| match part {
                    llm::completion::message::UserContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_else(|| "empty".to_string());

        Ok(ModelResponse::text(text, Usage::default()))
    }
}

/// Verifies child agents run on dedicated threads and notify the caller through the mailbox.
#[tokio::test]
async fn collaboration_runtime_spawns_waits_and_reuses_child_threads() {
    let root_dir = temp_root("collaboration-runtime");
    let store = Arc::new(InMemorySessionStore::default());
    let runtime = ThreadRuntime::new(
        Arc::new(EchoModel),
        Arc::clone(&store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::new(RecordingEventSink::default()),
    );

    let session_id = SessionId::new();
    let root_thread = ThreadHandle::new(session_id, ThreadId::new()).with_cwd(&root_dir);
    let collaboration = runtime.collaboration_runtime_for_thread(&root_thread);
    let origin = AgentRuntimeContext {
        cwd: Some(root_dir.to_string_lossy().to_string()),
        ..AgentRuntimeContext::default()
    };

    let first = collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            name: Some("writer".to_string()),
            task: Some("draft".to_string()),
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("spawn should succeed");
    let second = collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            name: Some("reviewer".to_string()),
            task: None,
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("second spawn should succeed");

    assert_eq!(first.agent.path, "1");
    assert_eq!(second.agent.path, "2");
    assert_ne!(first.agent.thread_id, root_thread.thread_id().to_string());

    let first_wait = collaboration
        .wait_agent(WaitAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            targets: vec![first.agent.path.clone()],
            timeout_ms: Some(5_000),
        })
        .await
        .expect("wait should succeed");
    let first_event = first_wait.event.expect("spawned task should finish");
    assert_eq!(first_event.event_kind, MailboxEventKind::Completed);
    assert_eq!(first_event.message, "draft");

    let send = collaboration
        .send_agent_input(SendAgentInputRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            target: first.agent.path.clone(),
            input: "revise".to_string(),
            interrupt: false,
        })
        .await
        .expect("send should succeed");
    assert_eq!(send.agent.status, AgentStatus::Running);

    let second_wait = collaboration
        .wait_agent(WaitAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            targets: vec![first.agent.agent_id.clone()],
            timeout_ms: Some(5_000),
        })
        .await
        .expect("second wait should succeed");
    let second_event = second_wait.event.expect("follow-up task should finish");
    assert_eq!(second_event.event_kind, MailboxEventKind::Completed);
    assert_eq!(second_event.message, "revise");

    let agents = collaboration
        .list_agents(ListAgentsRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
        })
        .await
        .expect("list should succeed");
    assert_eq!(agents.agents.len(), 2);
    assert!(
        agents
            .agents
            .iter()
            .any(|agent| agent.agent_id == first.agent.agent_id
                && agent.status == AgentStatus::Completed)
    );

    let close = collaboration
        .close_agent(CloseAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin,
            target: first.agent.agent_id,
        })
        .await
        .expect("close should succeed");
    assert_eq!(close.agent.status, AgentStatus::Closed);
}

/// Verifies child workers publish results through the mailbox only and do not
/// leak their internal event stream into the parent thread's event sink.
#[tokio::test]
async fn collaboration_runtime_child_worker_events_stay_out_of_parent_sink() {
    let root_dir = temp_root("collaboration-parent-sink");
    let store = Arc::new(InMemorySessionStore::default());
    let sink = Arc::new(RecordingEventSink::default());
    let runtime = ThreadRuntime::new(
        Arc::new(EchoModel),
        Arc::clone(&store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::clone(&sink),
    );

    let session_id = SessionId::new();
    let root_thread = ThreadHandle::new(session_id, ThreadId::new()).with_cwd(&root_dir);
    let collaboration = runtime.collaboration_runtime_for_thread(&root_thread);
    let origin = AgentRuntimeContext {
        cwd: Some(root_dir.to_string_lossy().to_string()),
        ..AgentRuntimeContext::default()
    };

    let spawned = collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            name: Some("writer".to_string()),
            task: Some("draft".to_string()),
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("spawn should succeed");

    let waited = collaboration
        .wait_agent(WaitAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin,
            targets: vec![spawned.agent.agent_id.clone()],
            timeout_ms: Some(5_000),
        })
        .await
        .expect("wait should succeed");
    assert_eq!(
        waited
            .event
            .expect("child should publish completion")
            .message,
        "draft"
    );

    let published_events = sink.snapshot().await;
    assert!(
        published_events.is_empty(),
        "child worker events should not leak into parent sink: {published_events:?}"
    );
}

/// Verifies visible agent paths are listed in numeric tree order rather than
/// plain lexicographic string order.
#[tokio::test]
async fn collaboration_runtime_lists_agents_in_numeric_path_order() {
    let root_dir = temp_root("collaboration-list-order");
    let store = Arc::new(InMemorySessionStore::default());
    let runtime = ThreadRuntime::new(
        Arc::new(EchoModel),
        Arc::clone(&store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::new(RecordingEventSink::default()),
    );

    let session_id = SessionId::new();
    let root_thread = ThreadHandle::new(session_id, ThreadId::new()).with_cwd(&root_dir);
    let collaboration = runtime.collaboration_runtime_for_thread(&root_thread);
    let origin = AgentRuntimeContext {
        cwd: Some(root_dir.to_string_lossy().to_string()),
        ..AgentRuntimeContext::default()
    };

    for index in 1..=10 {
        collaboration
            .spawn_agent(SpawnAgentRequest {
                session_id: session_id.to_string(),
                thread_id: root_thread.thread_id().to_string(),
                origin: origin.clone(),
                name: Some(format!("agent-{index}")),
                task: None,
                cwd: None,
                system_prompt: None,
                current_date: None,
                timezone: None,
            })
            .await
            .expect("spawn should succeed");
    }

    let listed = collaboration
        .list_agents(ListAgentsRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin,
        })
        .await
        .expect("list should succeed");
    let ordered_paths = listed
        .agents
        .iter()
        .map(|agent| agent.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ordered_paths,
        vec!["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]
    );
}

/// Verifies collaboration events are appended to the JSONL session store alongside turn events.
#[tokio::test]
async fn collaboration_runtime_persists_agent_graph_and_mailbox_events() {
    let root_dir = temp_root("collaboration-persistence");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let persist = JsonlSessionStore::create_at(&path).expect("create store");
    let store = Arc::new(InMemorySessionStore::default().with_persistence(Arc::new(persist)));
    let runtime = ThreadRuntime::new(
        Arc::new(EchoModel),
        Arc::clone(&store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::new(RecordingEventSink::default()),
    );

    let session_id = SessionId::new();
    let root_thread = ThreadHandle::new(session_id, ThreadId::new()).with_cwd(&root_dir);
    let collaboration = runtime.collaboration_runtime_for_thread(&root_thread);

    collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: AgentRuntimeContext {
                cwd: Some(root_dir.to_string_lossy().to_string()),
                ..AgentRuntimeContext::default()
            },
            name: Some("writer".to_string()),
            task: Some("draft".to_string()),
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("spawn should succeed");

    let waited = collaboration
        .wait_agent(WaitAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: AgentRuntimeContext {
                cwd: Some(root_dir.to_string_lossy().to_string()),
                ..AgentRuntimeContext::default()
            },
            targets: Vec::new(),
            timeout_ms: Some(5_000),
        })
        .await
        .expect("wait should succeed");
    assert!(waited.event.is_some());

    drop(runtime);
    drop(store);

    let events = load_session_events(&path).expect("load events");
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::AgentRegistered { name: Some(name), .. } if name == "writer"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::AgentStatusChanged { status, .. } if status.as_str() == "completed"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::MailboxDelivered { event_kind, message, .. }
            if event_kind.as_str() == "completed" && message == "draft"
    )));
}

/// Verifies collaboration replay restores visible agents, unread mailbox events, and idle child workers.
#[tokio::test]
async fn collaboration_runtime_replays_agents_mailbox_and_idle_children() {
    let root_dir = temp_root("collaboration-replay");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let persist = JsonlSessionStore::create_at(&path).expect("create store");
    let store = Arc::new(InMemorySessionStore::default().with_persistence(Arc::new(persist)));
    let runtime = ThreadRuntime::new(
        Arc::new(EchoModel),
        Arc::clone(&store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::new(RecordingEventSink::default()),
    );

    let session_id = SessionId::new();
    let root_thread = ThreadHandle::new(session_id, ThreadId::new()).with_cwd(&root_dir);
    let collaboration = runtime.collaboration_runtime_for_thread(&root_thread);
    let origin = AgentRuntimeContext {
        cwd: Some(root_dir.to_string_lossy().to_string()),
        ..AgentRuntimeContext::default()
    };

    store
        .begin_turn_state(
            session_id,
            root_thread.thread_id().clone(),
            "supervise".to_string(),
            Message::user("supervise"),
        )
        .await
        .expect("root turn should start");
    store
        .append_message_state(
            session_id,
            root_thread.thread_id().clone(),
            Message::assistant("ready"),
        )
        .await
        .expect("root turn assistant message should persist");
    let mut root_context = TurnContext::new(session_id, root_thread.thread_id().clone());
    root_context.cwd = Some(root_dir.to_string_lossy().to_string());
    store
        .finalize_turn_state(&root_context, Usage::default())
        .await
        .expect("root turn should finalize");

    let writer = collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            name: Some("writer".to_string()),
            task: Some("draft".to_string()),
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("writer spawn should succeed");
    let reviewer = collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            name: Some("reviewer".to_string()),
            task: None,
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("reviewer spawn should succeed");

    for _ in 0..50 {
        let listed = collaboration
            .list_agents(ListAgentsRequest {
                session_id: session_id.to_string(),
                thread_id: root_thread.thread_id().to_string(),
                origin: origin.clone(),
            })
            .await
            .expect("list should succeed");
        if listed.agents.iter().any(|agent| {
            agent.agent_id == writer.agent.agent_id && agent.status == AgentStatus::Completed
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    drop(runtime);
    drop(store);

    let events = load_session_events(&path).expect("load events");
    let resumed_store = Arc::new(InMemorySessionStore::default());
    let collaboration_session = CollaborationSession::new(Arc::clone(&resumed_store));
    let (loaded_sid, loaded_tid) = resumed_store
        .load_from_events(events.clone())
        .await
        .expect("replay turns");
    collaboration_session
        .replay_events(&events)
        .await
        .expect("replay collaboration");

    let resumed_runtime = ThreadRuntime::new_with_collaboration_session(
        Arc::new(EchoModel),
        Arc::clone(&resumed_store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::new(RecordingEventSink::default()),
        collaboration_session,
    );
    let resumed_thread = ThreadHandle::new(loaded_sid, loaded_tid.clone()).with_cwd(&root_dir);
    let resumed_collaboration = resumed_runtime.collaboration_runtime_for_thread(&resumed_thread);
    let resumed_origin = AgentRuntimeContext {
        cwd: Some(root_dir.to_string_lossy().to_string()),
        ..AgentRuntimeContext::default()
    };

    let agents = resumed_collaboration
        .list_agents(ListAgentsRequest {
            session_id: loaded_sid.to_string(),
            thread_id: loaded_tid.to_string(),
            origin: resumed_origin.clone(),
        })
        .await
        .expect("resumed list should succeed");
    assert_eq!(agents.agents.len(), 2);
    assert!(
        agents
            .agents
            .iter()
            .any(|agent| agent.agent_id == writer.agent.agent_id
                && agent.status == AgentStatus::Completed)
    );
    assert!(agents.agents.iter().any(
        |agent| agent.agent_id == reviewer.agent.agent_id && agent.status == AgentStatus::Idle
    ));

    let unread = resumed_collaboration
        .wait_agent(WaitAgentRequest {
            session_id: loaded_sid.to_string(),
            thread_id: loaded_tid.to_string(),
            origin: resumed_origin.clone(),
            targets: vec![writer.agent.agent_id.clone()],
            timeout_ms: Some(100),
        })
        .await
        .expect("replayed mailbox should be readable");
    let unread_event = unread.event.expect("replayed mailbox event should exist");
    assert_eq!(unread_event.event_kind, MailboxEventKind::Completed);
    assert_eq!(unread_event.message, "draft");

    resumed_collaboration
        .send_agent_input(SendAgentInputRequest {
            session_id: loaded_sid.to_string(),
            thread_id: loaded_tid.to_string(),
            origin: resumed_origin.clone(),
            target: reviewer.agent.agent_id.clone(),
            input: "review".to_string(),
            interrupt: false,
        })
        .await
        .expect("resumed idle child should accept work");

    let resumed_wait = resumed_collaboration
        .wait_agent(WaitAgentRequest {
            session_id: loaded_sid.to_string(),
            thread_id: loaded_tid.to_string(),
            origin: resumed_origin,
            targets: vec![reviewer.agent.agent_id.clone()],
            timeout_ms: Some(5_000),
        })
        .await
        .expect("resumed idle child should finish work");
    let resumed_event = resumed_wait
        .event
        .expect("reviewer should publish completion");
    assert_eq!(resumed_event.event_kind, MailboxEventKind::Completed);
    assert_eq!(resumed_event.message, "review");
}

/// Verifies closing an agent clears queued tasks and prevents later task completions from reviving it.
#[tokio::test]
async fn collaboration_runtime_close_agent_stays_terminal_and_drops_queued_inputs() {
    let root_dir = temp_root("collaboration-close");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store = Arc::new(InMemorySessionStore::default());
    let runtime = ThreadRuntime::new(
        Arc::new(GatedEchoModel::new(
            Arc::clone(&started),
            Arc::clone(&release),
        )),
        Arc::clone(&store),
        Arc::new(tools::ToolRouter::from_path(&root_dir).await),
        Arc::new(RecordingEventSink::default()),
    );

    let session_id = SessionId::new();
    let root_thread = ThreadHandle::new(session_id, ThreadId::new()).with_cwd(&root_dir);
    let collaboration = runtime.collaboration_runtime_for_thread(&root_thread);
    let origin = AgentRuntimeContext {
        cwd: Some(root_dir.to_string_lossy().to_string()),
        ..AgentRuntimeContext::default()
    };

    let spawned = collaboration
        .spawn_agent(SpawnAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            name: Some("writer".to_string()),
            task: Some("first".to_string()),
            cwd: None,
            system_prompt: None,
            current_date: None,
            timezone: None,
        })
        .await
        .expect("spawn should succeed");

    started.notified().await;

    collaboration
        .send_agent_input(SendAgentInputRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            target: spawned.agent.agent_id.clone(),
            input: "second".to_string(),
            interrupt: false,
        })
        .await
        .expect("second input should queue");

    let close = collaboration
        .close_agent(CloseAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            target: spawned.agent.agent_id.clone(),
        })
        .await
        .expect("close should succeed");
    assert_eq!(close.agent.status, AgentStatus::Closed);

    let listed = collaboration
        .list_agents(ListAgentsRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
        })
        .await
        .expect("list should succeed");
    let closed = listed
        .agents
        .iter()
        .find(|agent| agent.agent_id == spawned.agent.agent_id)
        .expect("closed agent should still be visible");
    assert_eq!(closed.status, AgentStatus::Closed);
    assert_eq!(closed.pending_tasks, 0);

    release.notify_waiters();

    let first_wait = collaboration
        .wait_agent(WaitAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin: origin.clone(),
            targets: vec![spawned.agent.agent_id.clone()],
            timeout_ms: Some(5_000),
        })
        .await
        .expect("close event should be delivered");
    let first_event = first_wait
        .event
        .expect("close should publish one mailbox event");
    assert_eq!(first_event.event_kind, MailboxEventKind::Closed);

    let second_wait = collaboration
        .wait_agent(WaitAgentRequest {
            session_id: session_id.to_string(),
            thread_id: root_thread.thread_id().to_string(),
            origin,
            targets: vec![spawned.agent.agent_id.clone()],
            timeout_ms: Some(200),
        })
        .await
        .expect("post-close wait should succeed");
    assert!(second_wait.timed_out);
    assert!(second_wait.event.is_none());
}

/// Builds a unique temporary root directory for collaboration integration tests.
fn temp_root(label: &str) -> std::path::PathBuf {
    tools::test_temp_root("kernel-collaboration", label)
}
