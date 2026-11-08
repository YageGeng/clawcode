use kernel::SessionEventSinks;
use protocol::{AgentEventPayload, RunRequest, SessionTitle};

use super::fixture::{Route, TestKernel};

/// Returns every title-change payload recorded for one Session.
fn title_changes(
    test: &TestKernel,
    session: &protocol::SessionId,
) -> Vec<Option<String>> {
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
    assert_eq!(title_changes(&test, &session), [Some("Manual".to_string())]);
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
            .run_with_insertion(
                RunRequest {
                    session_id: running,
                    input: "hold".into(),
                },
                sink,
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
    assert_eq!(
        title_changes(&test, &session),
        [Some("hold".to_string()), Some("Live".to_string())]
    );
    let live = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                AgentEventPayload::SessionTitleChanged { title }
                    if title.as_deref() == Some("Live")
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
    assert_eq!(title_changes(&test, &session), [Some("Manual".to_string())]);
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

/// Renames the Session from an input hook, the way an extension titles a conversation.
struct ExtensionRename(Option<&'static str>);

#[async_trait::async_trait]
impl extension::ExtensionHandler<extension::InputPoint> for ExtensionRename {
    /// Sets the title through the extension host before input continues.
    async fn handle(
        &self,
        _event: &protocol::InputEvent,
        context: &extension::ExtensionContext,
    ) -> Result<protocol::InputResult, extension::ExtensionError> {
        context
            .set_session_name(self.0.map(str::to_string))
            .await
            .map_err(|error| {
                extension::ExtensionError::Handler(error.to_string())
            })?;
        Ok(protocol::InputResult::Continue)
    }
}

/// An extension rename reaches observers exactly like a client-set title.
#[tokio::test]
async fn extension_rename_announces_title() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("answer"),
        super::insertion::hooks(|registrar| {
            registrar.on::<extension::InputPoint, _>(ExtensionRename(Some(
                "From extension",
            )))
        }),
    );
    let session = test.create("title-extension").await;

    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "first prompt".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");

    // The extension's title wins over the default first-prompt title.
    assert_eq!(
        title_changes(&test, &session),
        [Some("From extension".to_string())]
    );
}

/// An extension clearing the name tells observers the title is gone.
#[tokio::test]
async fn extension_clear_announces_cleared_title() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("answer"),
        super::insertion::hooks(|registrar| {
            registrar.on::<extension::InputPoint, _>(ExtensionRename(None))
        }),
    );
    let session = test.create("title-cleared").await;
    test.kernel
        .set_session_title(&session, SessionTitle::try_from("Manual").unwrap())
        .await
        .expect("set title");

    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "first prompt".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");

    // The clear sticks: the first prompt does not fill in a default title.
    assert_eq!(test.kernel.session_tree(&session).expect("tree").name, None);
    assert_eq!(
        title_changes(&test, &session),
        [Some("Manual".to_string()), None]
    );
}

/// Holds Session shutdown inside its hook until the test releases it.
#[derive(Clone)]
struct BlockingShutdown {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl extension::ExtensionHandler<extension::SessionShutdownPoint>
    for BlockingShutdown
{
    /// Signals entry, then keeps the Session closing until released.
    async fn handle(
        &self,
        _event: &protocol::SessionShutdownEvent,
        _context: &extension::ExtensionContext,
    ) -> Result<(), extension::ExtensionError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

/// A title set while the Session is closing waits for shutdown, then lands in the Store.
#[tokio::test]
async fn set_session_title_waits_for_a_closing_session() {
    let shutdown = BlockingShutdown {
        entered: std::sync::Arc::new(tokio::sync::Notify::new()),
        release: std::sync::Arc::new(tokio::sync::Notify::new()),
    };
    let hook = shutdown.clone();
    let test = TestKernel::with_extensions(
        |_| Route::answer("answer"),
        super::insertion::hooks(move |registrar| {
            registrar.on::<extension::SessionShutdownPoint, _>(hook.clone())
        }),
    );
    let session = test.create("title-closing").await;
    let kernel = test.kernel.clone();
    let closing = session.clone();
    let close =
        tokio::spawn(async move { kernel.close_session(&closing).await });
    shutdown.entered.notified().await;

    let kernel = test.kernel.clone();
    let retitled = session.clone();
    let title = tokio::spawn(async move {
        kernel
            .set_session_title(
                &retitled,
                SessionTitle::try_from("Late").unwrap(),
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!title.is_finished(), "the title must wait for shutdown");

    shutdown.release.notify_one();
    close.await.unwrap().expect("close");
    title.await.unwrap().expect("title after close");
    let listed = test.kernel.list_sessions(None).expect("list");
    let summary = listed
        .iter()
        .find(|summary| summary.session_id == session)
        .expect("listed Session");
    assert_eq!(summary.name.as_deref(), Some("Late"));
}
