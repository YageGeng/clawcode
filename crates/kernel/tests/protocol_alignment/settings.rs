use protocol::{AgentEventPayload, ThinkingLevel};

use super::fixture::{Route, TestKernel};

/// Returns the settings carried by every `SettingsChanged` event a Session saw.
fn settings_events(
    test: &TestKernel,
    session: &protocol::SessionId,
) -> Vec<(String, ThinkingLevel)> {
    test.sinks
        .payloads(session)
        .into_iter()
        .filter_map(|payload| match payload {
            AgentEventPayload::SettingsChanged { settings } => {
                Some((settings.model.model_id, settings.thinking_level))
            }
            _ => None,
        })
        .collect()
}

/// A client model change is persisted, reported, and survives a resume.
#[tokio::test]
async fn set_session_model_persists_and_emits_settings_changed() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("settings-model").await;
    let settings = test.kernel.session_settings(&session).expect("settings");
    assert_eq!(settings.model.model_id, "primary");
    assert_eq!(settings.models.len(), 2);

    let selected = test
        .kernel
        .set_session_model(&session, "fixture", "secondary")
        .await
        .expect("set model");
    assert_eq!(selected.model_id, "secondary");
    let level = test
        .kernel
        .set_session_thinking_level(&session, ThinkingLevel::High)
        .await
        .expect("set thinking");
    assert_eq!(level, ThinkingLevel::High);
    assert_eq!(
        settings_events(&test, &session),
        vec![
            ("secondary".to_string(), ThinkingLevel::Off),
            ("secondary".to_string(), ThinkingLevel::High),
        ]
    );

    test.kernel
        .close_session(&session)
        .await
        .expect("close Session");
    test.kernel
        .resume_session(session.clone(), test.root.path().to_path_buf())
        .await
        .expect("resume Session");
    let restored = test.kernel.session_settings(&session).expect("settings");
    assert_eq!(restored.model.model_id, "secondary");
    assert_eq!(restored.thinking_level, ThinkingLevel::High);
}

/// Selecting the current value changes nothing and reports nothing.
#[tokio::test]
async fn unchanged_setting_emits_nothing() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("settings-unchanged").await;
    test.kernel
        .set_session_model(&session, "fixture", "primary")
        .await
        .expect("same model");
    assert!(settings_events(&test, &session).is_empty());
}

/// Changes an extension makes go through the same reported path.
#[tokio::test]
async fn extension_model_change_emits_settings_changed() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("settings-extension").await;
    test.kernel
        .invoke_extension_command(
            &session,
            "agent-state".to_string(),
            serde_json::json!({}),
        )
        .await
        .expect("extension command");
    assert_eq!(
        settings_events(&test, &session).last(),
        Some(&("secondary".to_string(), ThinkingLevel::High))
    );
}

/// A model outside the catalog is rejected without changing the Session.
#[tokio::test]
async fn unknown_model_is_rejected() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("settings-unknown").await;
    test.kernel
        .set_session_model(&session, "fixture", "missing")
        .await
        .expect_err("unknown model");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .model
            .model_id,
        "primary"
    );
    assert!(settings_events(&test, &session).is_empty());
}
