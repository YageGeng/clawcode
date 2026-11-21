//! In-process ACP client: session lifecycle requests and backend startup failure.

mod support;

use std::time::Duration;

use agent_client_protocol::v2::schema as wire;
use config::{
    AppConfig, ConfigHandle, ExtensionsConfig, SessionPersistenceConfig,
};
use desktop::backend::{AgentRequest, Backend, BackendError, QuestionKey};
use protocol::{ExtensionId, WorkspaceFileContent, WorkspaceGitStatus};
use support::ScriptedBackend;

/// Serializes updates so assertions can search their text.
fn json(updates: &[wire::SessionUpdate]) -> String {
    serde_json::to_string(updates).expect("serialize updates")
}

#[test]
fn new_session_is_listed_with_its_cwd() {
    let backend = ScriptedBackend::start();
    let id = backend.new_session();

    let sessions = backend
        .block_on(backend.client.list_sessions())
        .expect("list");

    let listed = sessions
        .iter()
        .find(|session| session.session_id == id)
        .expect("listed");
    assert_eq!(
        AsRef::<std::path::Path>::as_ref(&listed.cwd),
        backend.workspace.path()
    );
}

#[test]
fn prompt_streams_updates_until_idle() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();

    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");
    let updates = backend.wait_idle(&id);

    let text = json(&updates);
    assert!(text.contains("agent_message"), "{text}");
    assert!(text.contains("Hi ") && text.contains("there"), "{text}");
}

#[test]
fn resume_with_replay_resends_history() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");
    backend.wait_idle(&id);
    backend
        .block_on(backend.client.close_session(&id))
        .expect("close");
    backend.drain(&id);

    backend
        .block_on(
            backend
                .client
                .resume_with_replay(&id, backend.workspace.path()),
        )
        .expect("resume");
    let replayed = json(&backend.wait_replay_complete(&id));

    assert!(replayed.contains("\"user_message\""), "{replayed}");
    assert!(replayed.contains("hello"), "{replayed}");
    assert!(replayed.contains("Hi there"), "{replayed}");
}

#[test]
fn session_running_reports_active_run() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();

    backend
        .block_on(backend.client.prompt(&id, vec!["slow".into()]))
        .expect("prompt");
    backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(_))
        )
    });
    assert!(
        backend
            .block_on(backend.client.session_running(&id))
            .expect("runtime")
    );

    backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))
        )
    });
    assert!(
        !backend
            .block_on(backend.client.session_running(&id))
            .expect("runtime")
    );
}

#[test]
fn set_title_and_delete_are_reflected_in_list() {
    let backend = ScriptedBackend::start();
    let id = backend.new_session();

    backend
        .block_on(backend.client.set_title(&id, "Renamed"))
        .expect("title");
    let sessions = backend
        .block_on(backend.client.list_sessions())
        .expect("list");
    let listed = sessions
        .iter()
        .find(|session| session.session_id == id)
        .expect("listed");
    assert_eq!(listed.title.as_deref(), Some("Renamed"));

    backend
        .block_on(backend.client.delete_session(&id))
        .expect("delete");
    let sessions = backend
        .block_on(backend.client.list_sessions())
        .expect("list");
    assert!(sessions.iter().all(|session| session.session_id != id));
}

#[test]
fn startup_failure_returns_error_without_hanging() {
    let home = tempfile::tempdir().expect("data home");
    let config = AppConfig {
        session_persistence: SessionPersistenceConfig {
            data_home: Some(home.path().to_str().expect("utf8").to_owned()),
        },
        extensions: ExtensionsConfig {
            enabled: vec![
                ExtensionId::try_from("hook-examples").expect("extension id"),
            ],
        },
        ..AppConfig::default()
    };
    let runtime = tokio::runtime::Runtime::new().expect("runtime");

    // The timer must be created inside the runtime, so build it in the async block.
    let result = runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                Backend::start(ConfigHandle::from_config(config)),
            )
            .await
        })
        .expect("start must not hang");

    match result {
        Err(BackendError::Startup(message)) => {
            assert!(message.contains("hook-examples"), "{message}")
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("an uncompiled extension must fail startup"),
    }
}

#[test]
fn pending_queue_round_trip() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["slow".into()]))
        .expect("prompt");
    backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(_))
        )
    });
    // A queued prompt answers only once it has run, so it must not block the test.
    let client = backend.client.clone();
    let queued = id.clone();
    backend.runtime.spawn(async move {
        let _ = client.prompt(&queued, vec!["hello".into()]).await;
    });

    let pending = backend.wait_follow_up(&id);
    assert_eq!(pending.follow_up.len(), 1, "{pending:?}");
    let queue_id = pending.follow_up[0].queue_id.clone();

    assert!(
        backend
            .block_on(backend.client.promote_pending(&id, &queue_id))
            .expect("promote")
    );
    let pending = backend
        .block_on(backend.client.pending_messages(&id))
        .expect("pending");
    assert_eq!((pending.steering.len(), pending.follow_up.len()), (1, 0));

    backend
        .block_on(backend.client.remove_pending(&id, &queue_id))
        .expect("remove");
    let pending = backend
        .block_on(backend.client.pending_messages(&id))
        .expect("pending");
    assert!(pending.steering.is_empty() && pending.follow_up.is_empty());
}

/// Switches the session into plan mode.
fn enter_plan_mode(
    backend: &ScriptedBackend,
    id: &desktop::backend::SessionId,
) {
    backend
        .block_on(backend.client.set_config_option(id, "mode", "plan"))
        .expect("plan mode");
}

/// Sends one text prompt.
fn say(
    backend: &ScriptedBackend,
    id: &desktop::backend::SessionId,
    text: &str,
) {
    backend
        .block_on(backend.client.prompt(
            id,
            vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
        ))
        .expect("prompt");
}

#[test]
fn plan_approval_reaches_the_client_and_approve_executes() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    enter_plan_mode(&backend, &id);

    say(&backend, &id, "plan");
    let request = backend.wait_request();

    let AgentRequest::Permission {
        id: request_id,
        request,
    } = request
    else {
        panic!("expected a permission request");
    };
    assert_eq!(request.title, "Ready to execute the plan?");
    backend
        .block_on(async {
            backend
                .client
                .answer_permission(request_id, Some("approve"))
        })
        .expect("answer");
    let updates = backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))
        )
    });
    assert!(json(&updates).contains("\"completed\""));
}

#[test]
fn cancelled_request_emits_request_cancelled() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    enter_plan_mode(&backend, &id);
    say(&backend, &id, "plan");
    let request = backend.wait_request();

    backend.client.cancel(&id).expect("cancel");
    backend.wait_request_cancelled(request.id());

    // Answering a withdrawn request is a harmless no-op.
    backend
        .block_on(async {
            backend
                .client
                .answer_permission(request.id(), Some("approve"))
        })
        .expect("late answer");
}

#[test]
fn set_config_option_returns_the_new_list() {
    let backend = ScriptedBackend::start();
    let id = backend.new_session();

    let options = backend
        .block_on(backend.client.set_config_option(
            &id,
            "thinking_level",
            "high",
        ))
        .expect("set");

    let thinking = options
        .iter()
        .find(|option| option.config_id.to_string() == "thinking_level")
        .expect("thinking option");
    assert!(
        serde_json::to_string(thinking)
            .expect("json")
            .contains("\"high\"")
    );
}

#[test]
fn set_config_option_rejects_unknown_values() {
    let backend = ScriptedBackend::start();
    let id = backend.new_session();

    let result = backend.block_on(backend.client.set_config_option(
        &id,
        "model",
        "nope/none",
    ));

    assert!(matches!(result, Err(BackendError::Request(_))));
}

#[test]
fn compact_runs_and_reports_completion() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "hello");
    backend.wait_idle(&id);

    backend
        .block_on(backend.client.compact(&id))
        .expect("compact");

    backend.wait_for(&id, |update| {
        matches!(update, wire::SessionUpdate::CompactionUpdate(_))
            && serde_json::to_string(update)
                .is_ok_and(|text| text.contains("\"completed\""))
    });
}

#[test]
fn question_answer_and_dismiss_round_trip() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "ask");
    let updates = backend.wait_idle(&id);
    let request_id = serde_json::to_value(&updates)
        .expect("json")
        .pointer_find("request_id")
        .expect("question request id");
    let first = QuestionKey {
        request_id: request_id.clone(),
        index: 0,
    };
    let second = QuestionKey {
        request_id,
        index: 1,
    };

    backend
        .block_on(backend.client.answer_question(&id, &first, "a"))
        .expect("answer");
    backend
        .block_on(
            backend
                .client
                .dismiss_questions(&id, std::slice::from_ref(&second)),
        )
        .expect("dismiss");
    backend.wait_idle(&id);

    let again =
        backend.block_on(backend.client.answer_question(&id, &first, "b"));
    assert!(matches!(again, Err(BackendError::QuestionClosed)));
}

#[test]
fn new_session_carries_config_options() {
    let backend = ScriptedBackend::start();

    let start = backend
        .block_on(backend.client.new_session(backend.workspace.path()))
        .expect("new session");

    let ids: Vec<String> = start
        .config_options
        .iter()
        .map(|option| option.config_id.to_string())
        .collect();
    assert_eq!(ids, ["model", "thinking_level", "mode"]);
}

/// Finds the first string value stored under `key` anywhere in a JSON tree.
trait PointerFind {
    /// Depth-first search for `key`.
    fn pointer_find(&self, key: &str) -> Option<String>;
}

impl PointerFind for serde_json::Value {
    fn pointer_find(&self, key: &str) -> Option<String> {
        match self {
            serde_json::Value::Object(map) => map
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    map.values().find_map(|value| value.pointer_find(key))
                }),
            serde_json::Value::Array(items) => {
                items.iter().find_map(|value| value.pointer_find(key))
            }
            _ => None,
        }
    }
}

#[test]
fn workspace_tools_round_trip_through_the_client() {
    let backend = ScriptedBackend::start();
    std::fs::write(backend.workspace.path().join("notes.md"), "# hi")
        .expect("file");
    let root = backend.workspace.path();

    let listing = backend
        .block_on(backend.client.list_dir(root, "", false))
        .expect("list");
    assert!(listing.entries.iter().any(|entry| entry.name == "notes.md"));
    assert_eq!(
        backend
            .block_on(backend.client.read_file(root, "notes.md"))
            .expect("read"),
        WorkspaceFileContent::Text {
            text: "# hi".into()
        }
    );
    assert!(matches!(
        backend
            .block_on(backend.client.git_status(root))
            .expect("status"),
        WorkspaceGitStatus::NotRepository
            | WorkspaceGitStatus::Repository { .. }
    ));
    assert!(matches!(
        backend.block_on(backend.client.read_file(root, "../x")),
        Err(BackendError::Request(_))
    ));
}
