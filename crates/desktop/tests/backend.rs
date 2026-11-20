//! In-process ACP client: session lifecycle requests and backend startup failure.

mod support;

use std::time::Duration;

use agent_client_protocol::v2::schema as wire;
use config::{
    AppConfig, ConfigHandle, ExtensionsConfig, SessionPersistenceConfig,
};
use desktop::backend::{Backend, BackendError};
use protocol::ExtensionId;
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
