//! Session registry: attaching with replay, live routing, release, and backend stop.

mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use agent_client_protocol::v2::schema as wire;
use desktop::backend::{BackendEvent, SessionId, SessionStart};
use desktop::catalog::SessionSummary;
use desktop::projection::{Role, TranscriptRow};
use desktop::session::{
    AttachPhase, RegistryEvent, SessionRegistry, SessionStatus,
};
use futures::channel::mpsc::unbounded;
use gpui_kit::{App, AppContext as _, Entity, TestAppContext};
use support::{ScriptedBackend, eventually};

/// Starts a registry whose pump consumes the fixture's events.
fn registry(
    cx: &mut TestAppContext,
    backend: &mut ScriptedBackend,
) -> Entity<SessionRegistry> {
    cx.executor().allow_parking();
    let events = std::mem::replace(&mut backend.events, unbounded().1);
    let registry = cx.new(|_| SessionRegistry::new(backend.client.clone()));
    let weak = registry.downgrade();
    cx.update(|cx| SessionRegistry::pump(weak, events, cx).detach());
    registry
}

/// Summary of a fixture session.
fn summary(backend: &ScriptedBackend, id: &SessionId) -> SessionSummary {
    SessionSummary::builder()
        .session_id(id.clone())
        .cwd(backend.workspace.path().to_path_buf())
        .build()
}

/// (role, text) of every message row of an attached session.
fn messages(
    registry: &Entity<SessionRegistry>,
    id: &SessionId,
    cx: &App,
) -> Vec<(Role, String)> {
    let Some(handle) = registry.read(cx).handle(id) else {
        return Vec::new();
    };
    let projection = handle.read(cx).projection();
    (0..projection.len())
        .filter_map(|index| match projection.row(index) {
            Some(TranscriptRow::Message(row)) => {
                Some((row.role, row.text.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Records every registry event.
fn record(
    cx: &mut TestAppContext,
    registry: &Entity<SessionRegistry>,
) -> Rc<RefCell<Vec<RegistryEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(registry, move |_, event: &RegistryEvent, _| {
            sink.borrow_mut().push(event.clone())
        })
        .detach()
    });
    seen
}

#[gpui_kit::test]
fn attach_replays_history_into_projection(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");
    backend.wait_idle(&id);
    let registry = registry(cx, &mut backend);

    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });

    eventually(cx, "replayed transcript", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
            && messages(&registry, &id, cx)
                == [
                    (Role::User, "hello".to_string()),
                    (Role::Assistant, "Hi there".to_string()),
                ]
    });
}

#[gpui_kit::test]
fn attaching_twice_returns_the_same_handle(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);

    let first = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    let second = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });

    assert_eq!(first.entity_id(), second.entity_id());
}

#[gpui_kit::test]
fn live_updates_reach_an_attached_session(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });

    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");

    eventually(cx, "live reply and idle", |cx| {
        messages(&registry, &id, cx)
            .contains(&(Role::Assistant, "Hi there".to_string()))
            && handle.read(cx).status() == SessionStatus::Idle
    });
}

#[gpui_kit::test]
fn events_for_unknown_sessions_are_ignored(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let registry = registry(cx, &mut backend);
    let stray = wire::UpdateSessionNotification::new(
        "nope".to_string(),
        serde_json::from_value(serde_json::json!({ "sessionUpdate": "state_update", "state": "running" }))
            .expect("update"),
    );

    registry.update(cx, |registry, cx| {
        registry.apply_batch(vec![BackendEvent::Update(Box::new(stray))], cx)
    });

    let nope = SessionId::from("nope".to_string());
    assert!(cx.update(|cx| registry.read(cx).handle(&nope).is_none()));
}

#[gpui_kit::test]
fn releasing_an_idle_session_closes_it(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let seen = record(cx, &registry);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });

    registry.update(cx, |registry, cx| registry.release(&id, cx));

    assert!(cx.update(|cx| registry.read(cx).handle(&id).is_none()));
    assert!(seen.borrow().contains(&RegistryEvent::Released(id.clone())));
}

#[gpui_kit::test]
fn releasing_a_running_session_waits_for_idle(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });
    backend
        .block_on(backend.client.prompt(&id, vec!["slow".into()]))
        .expect("prompt");
    eventually(cx, "running", |cx| {
        handle.read(cx).status() == SessionStatus::Running
    });
    assert_eq!(cx.update(|cx| registry.read(cx).running_count(cx)), 1);

    registry.update(cx, |registry, cx| registry.release(&id, cx));
    assert!(cx.update(|cx| registry.read(cx).handle(&id).is_some()));

    eventually(cx, "release after idle", |cx| {
        registry.read(cx).handle(&id).is_none()
    });
}

#[gpui_kit::test]
fn open_questions_alone_do_not_count_as_running(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });
    backend
        .block_on(backend.client.prompt(&id, vec!["ask".into()]))
        .expect("prompt");
    eventually(cx, "questions open and run settled", |cx| {
        let handle = handle.read(cx);
        !handle.projection().open_questions().is_empty()
            && handle.projection().run_state()
                == desktop::projection::RunState::Idle
    });

    // The session waits for the user, but quitting would stop no run.
    assert_eq!(
        handle.read_with(cx, |handle, _| handle.status()),
        SessionStatus::Waiting
    );
    assert_eq!(cx.update(|cx| registry.read(cx).running_count(cx)), 0);
}

#[gpui_kit::test]
fn backend_stop_is_reported(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let registry = registry(cx, &mut backend);
    let seen = record(cx, &registry);

    registry.update(cx, |registry, cx| {
        registry
            .apply_batch(vec![BackendEvent::Stopped(Some("x".to_string()))], cx)
    });
    cx.run_until_parked();

    assert_eq!(
        *seen.borrow(),
        vec![RegistryEvent::BackendStopped(Some("x".to_string()))]
    );
}

#[gpui_kit::test]
fn forgetting_a_session_drops_it_without_closing(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let seen = record(cx, &registry);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });

    registry.update(cx, |registry, _| registry.forget(&id));
    cx.run_until_parked();

    assert!(cx.update(|cx| registry.read(cx).handle(&id).is_none()));
    assert!(
        seen.borrow().is_empty(),
        "forget must not close: {:?}",
        seen.borrow()
    );
    backend
        .block_on(backend.client.delete_session(&id))
        .expect("delete right after forget");
}

#[gpui_kit::test]
fn reattaching_a_running_session_cancels_its_pending_close(
    cx: &mut TestAppContext,
) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });
    backend
        .block_on(backend.client.prompt(&id, vec!["slow".into()]))
        .expect("prompt");
    eventually(cx, "running", |cx| {
        handle.read(cx).status() == SessionStatus::Running
    });

    registry.update(cx, |registry, cx| registry.release(&id, cx));
    let again = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "idle", |cx| {
        again.read(cx).status() == SessionStatus::Idle
            && !messages(&registry, &id, cx).is_empty()
    });
    cx.run_until_parked();

    assert!(
        cx.update(|cx| registry.read(cx).handle(&id).is_some()),
        "reopened session was closed when its run ended"
    );
}

#[gpui_kit::test]
fn reopening_right_after_close_attaches_cleanly(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    for round in 0..8 {
        let handle = registry.update(cx, |registry, cx| {
            registry.attach(&summary(&backend, &id), cx)
        });
        eventually(cx, "attach", |cx| {
            handle.read(cx).phase() != &AttachPhase::Attaching
        });
        assert_eq!(
            cx.update(|cx| handle.read(cx).phase().clone()),
            AttachPhase::Attached,
            "round {round}"
        );
        registry.update(cx, |registry, cx| registry.release(&id, cx));
    }
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });

    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");
    eventually(cx, "live reply after reopen", |cx| {
        messages(&registry, &id, cx)
            .contains(&(Role::Assistant, "Hi there".to_string()))
    });
}

#[gpui_kit::test]
fn an_ended_event_stream_reports_the_backend_stopped(cx: &mut TestAppContext) {
    let backend = ScriptedBackend::start();
    cx.executor().allow_parking();
    let (sender, events) = unbounded();
    let registry = cx.new(|_| SessionRegistry::new(backend.client.clone()));
    let seen = record(cx, &registry);
    let weak = registry.downgrade();
    cx.update(|cx| SessionRegistry::pump(weak, events, cx).detach());

    drop(sender);
    cx.run_until_parked();

    assert_eq!(*seen.borrow(), vec![RegistryEvent::BackendStopped(None)]);
}

#[gpui_kit::test]
fn replay_complete_from_the_server_finishes_attaching(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    assert_eq!(
        cx.update(|cx| handle.read(cx).phase().clone()),
        AttachPhase::Attaching
    );

    let complete = wire::UpdateSessionNotification::new(
        id.clone(),
        wire::SessionUpdate::ReplayComplete(wire::ReplayComplete::new()),
    );
    registry.update(cx, |registry, cx| {
        registry.apply_batch(vec![BackendEvent::Update(Box::new(complete))], cx)
    });

    // Decided by the server's marker, before the resume response is even read.
    assert_eq!(
        cx.update(|cx| handle.read(cx).phase().clone()),
        AttachPhase::Attached
    );
}

#[gpui_kit::test]
fn adopted_session_streams_without_replay(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);

    let handle = registry.update(cx, |registry, cx| {
        registry.adopt(
            SessionStart {
                id: id.clone(),
                config_options: Vec::new(),
            },
            backend.workspace.path(),
            cx,
        )
    });
    assert_eq!(
        handle.read_with(cx, |h, _| h.phase().clone()),
        AttachPhase::Attached
    );

    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");
    eventually(cx, "live reply", |cx| {
        messages(&registry, &id, cx)
            .contains(&(Role::Assistant, "Hi there".to_string()))
    });
}

#[gpui_kit::test]
fn attach_loads_the_pending_queue(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["slower".into()]))
        .expect("prompt");
    // A queued prompt answers only once it has run, so it must not block the test.
    let client = backend.client.clone();
    let queued = id.clone();
    backend.runtime.spawn(async move {
        let _ = client.prompt(&queued, vec!["hello".into()]).await;
    });
    backend.wait_follow_up(&id);
    let registry = registry(cx, &mut backend);

    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });

    eventually(cx, "pending queue", |cx| {
        handle.read(cx).projection().pending().follow_up.len() == 1
    });
}

#[gpui_kit::test]
fn title_and_settle_emit_session_changed(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.adopt(
            SessionStart {
                id: id.clone(),
                config_options: Vec::new(),
            },
            backend.workspace.path(),
            cx,
        )
    });
    let seen = record(cx, &registry);

    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("prompt");
    eventually(cx, "idle reply", |cx| {
        messages(&registry, &id, cx)
            .contains(&(Role::Assistant, "Hi there".to_string()))
            && handle.read(cx).status() == SessionStatus::Idle
    });
    std::thread::sleep(Duration::from_millis(100));
    cx.run_until_parked();

    let changed = seen
        .borrow()
        .iter()
        .filter(|event| **event == RegistryEvent::SessionChanged(id.clone()))
        .count();
    // The new title and the settled run each mark the catalog stale; they
    // share one event when they land in one batch and take two otherwise.
    assert!((1..=2).contains(&changed), "{:?}", seen.borrow());
}

#[gpui_kit::test]
fn pending_request_marks_the_session_waiting(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.set_config_option(&id, "mode", "plan"))
        .expect("plan mode");
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });

    backend
        .block_on(backend.client.prompt(
            &id,
            vec![wire::ContentBlock::Text(wire::TextContent::new("plan"))],
        ))
        .expect("prompt");

    eventually(cx, "approval request", |cx| {
        handle.read(cx).projection().controls().requests().len() == 1
            && handle.read(cx).status() == SessionStatus::Waiting
    });
    backend.client.cancel(&id).expect("cancel");
    eventually(cx, "request withdrawn", |cx| {
        handle
            .read(cx)
            .projection()
            .controls()
            .requests()
            .is_empty()
            && handle.read(cx).status() != SessionStatus::Waiting
    });
}

#[gpui_kit::test]
fn releasing_a_session_with_only_open_questions_closes_it(
    cx: &mut TestAppContext,
) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });
    backend
        .block_on(backend.client.prompt(&id, vec!["ask".into()]))
        .expect("prompt");
    eventually(cx, "questions open and run settled", |cx| {
        let handle = handle.read(cx);
        !handle.projection().open_questions().is_empty()
            && handle.projection().run_state()
                == desktop::projection::RunState::Idle
    });

    // The questions come back on the next attach, so nothing keeps it open.
    registry.update(cx, |registry, cx| registry.release(&id, cx));
    assert!(cx.update(|cx| registry.read(cx).handle(&id).is_none()));
}

#[gpui_kit::test]
fn releasing_a_session_declines_its_pending_request(cx: &mut TestAppContext) {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.set_config_option(&id, "mode", "plan"))
        .expect("plan mode");
    let registry = registry(cx, &mut backend);
    let handle = registry.update(cx, |registry, cx| {
        registry.attach(&summary(&backend, &id), cx)
    });
    eventually(cx, "attach", |cx| {
        handle.read(cx).phase() == &AttachPhase::Attached
    });
    backend
        .block_on(backend.client.prompt(
            &id,
            vec![wire::ContentBlock::Text(wire::TextContent::new("plan"))],
        ))
        .expect("prompt");
    eventually(cx, "approval request", |cx| {
        handle.read(cx).projection().controls().requests().len() == 1
    });

    // No tab can answer the approval any more: it is declined, the run
    // ends, and the session closes instead of waiting forever.
    registry.update(cx, |registry, cx| registry.release(&id, cx));
    eventually(cx, "closed after the declined run ends", |cx| {
        registry.read(cx).handle(&id).is_none()
    });
}
