use kernel::SessionEventSinks;
use protocol::{AgentEventPayload, RunRequest, SessionTitle, TraceId};

use super::fixture::{Route, TestKernel};

/// Returns every title-change payload recorded for one Session.
fn title_changes(
    test: &TestKernel,
    session: &protocol::SessionId,
) -> Vec<String> {
    test.sinks
        .payloads(session)
        .into_iter()
        .filter_map(|payload| match payload {
            AgentEventPayload::SessionTitleChanged { title } => Some(title),
            _ => None,
        })
        .collect()
}

/// A client-set title is announced once, while an internal rename stays silent.
#[tokio::test]
async fn set_session_title_emits_one_event_when_idle() {
    let test = TestKernel::new(|_| Route::answer("answer"));
    let session = test.create("title-idle").await;

    test.kernel
        .rename_session(&session, SessionTitle::try_from("Quiet").unwrap())
        .await
        .expect("rename");
    assert!(title_changes(&test, &session).is_empty());

    test.kernel
        .set_session_title(&session, SessionTitle::try_from("Manual").unwrap())
        .await
        .expect("set title");
    assert_eq!(title_changes(&test, &session), ["Manual"]);
}

/// A title set during a Run is attributed to that Run's active Turn.
#[tokio::test]
async fn set_session_title_uses_active_turn_while_running() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("title-live").await;
    let kernel = test.kernel.clone();
    let sink = test.sinks.sink(&session);
    let running = session.clone();
    let run = tokio::spawn(async move {
        kernel
            .run_traced(
                RunRequest {
                    session_id: running,
                    input: "hold".into(),
                },
                sink,
                TraceId::try_from("trace-title").unwrap(),
                None,
            )
            .await
    });
    test.wait_until(|| {
        test.sinks.saw(&session, |payload| {
            matches!(payload, AgentEventPayload::TurnStart { .. })
        })
    })
    .await;

    test.kernel
        .set_session_title(&session, SessionTitle::try_from("Live").unwrap())
        .await
        .expect("set title");

    let events = test.sinks.events(&session);
    let turn = events
        .iter()
        .find(|event| {
            matches!(event.payload, AgentEventPayload::TurnStart { .. })
        })
        .map(|event| event.metadata.turn_id.clone())
        .expect("turn start");
    // The Run's first input sets the default title before the client's title.
    assert_eq!(title_changes(&test, &session), ["hold", "Live"]);
    let live = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                AgentEventPayload::SessionTitleChanged { title } if title == "Live"
            )
        })
        .expect("client title event");
    assert_eq!(live.metadata.turn_id, turn);

    test.kernel.cancel_session(&session).unwrap();
    let _cancelled = run.await.unwrap();
}

/// The first prompt's default title never replaces a client-set title.
#[tokio::test]
async fn set_title_is_not_replaced_by_default_title() {
    let test = TestKernel::new(|_| Route::answer("answer"));
    let session = test.create("title-kept").await;
    test.kernel
        .set_session_title(&session, SessionTitle::try_from("Manual").unwrap())
        .await
        .expect("set title");

    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "First line".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");

    assert_eq!(
        test.kernel
            .session_tree(&session)
            .expect("tree")
            .name
            .as_deref(),
        Some("Manual")
    );
    assert_eq!(title_changes(&test, &session), ["Manual"]);
}

/// A persisted Session that is listed but not loaded can still be retitled.
#[tokio::test]
async fn set_session_title_renames_a_persisted_session_that_is_not_loaded() {
    let test = TestKernel::new(|_| Route::answer("answer"));
    let session = test.create("title-cold").await;
    test.kernel.close_session(&session).await.expect("close");

    test.kernel
        .set_session_title(&session, SessionTitle::try_from(" Cold ").unwrap())
        .await
        .expect("set title on an unloaded Session");

    let listed = test.kernel.list_sessions(None).expect("list");
    let summary = listed
        .iter()
        .find(|summary| summary.session_id == session)
        .expect("listed Session");
    assert_eq!(summary.name.as_deref(), Some("Cold"));
    test.kernel
        .resume_session(session.clone(), test.root.path().into())
        .await
        .expect("resume");
    assert_eq!(
        test.kernel
            .session_tree(&session)
            .expect("tree")
            .name
            .as_deref(),
        Some("Cold")
    );
}
