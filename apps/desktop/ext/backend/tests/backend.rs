//! The in-process backend thread behind the `Backend` class: frames in and out,
//! startup failure, and shutdown.

use std::time::Duration;

use clawcode_backend::BackendThread;
use config::{
    AppConfig, ConfigHandle, ExtensionsConfig, SessionPersistenceConfig,
};
use futures::StreamExt;

/// A configuration with an offline fixture provider whose data lives under `home`.
fn config_in(home: &tempfile::TempDir) -> ConfigHandle {
    let path = home.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            r#"
active_model = "fixture/model"

[session_persistence]
data_home = {:?}

[[providers]]
id = "fixture"
display_name = "Fixture"
base_url = "https://example.invalid"
api_key = "sk-test"

[[providers.models]]
id = "model"
display_name = "Fixture Model"
context_tokens = 32768
max_output_tokens = 1024
"#,
            home.path().join("data")
        ),
    )
    .expect("write config");
    config::load_from([path]).expect("load config")
}

/// An ACP `initialize` request frame with id 1.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":2,"info":{"name":"backend-test","version":"0.1.0"}}}"#;

/// The backend answers a frame with a frame.
#[tokio::test]
async fn start_answers_initialize_through_frames() {
    let home = tempfile::tempdir().expect("data home");
    let (mut backend, mut output) =
        BackendThread::start(config_in(&home)).await.expect("start");

    backend.send(INITIALIZE.to_owned()).expect("send");
    let reply =
        tokio::time::timeout(Duration::from_secs(10), output.frames.next())
            .await
            .expect("reply within 10s")
            .expect("a frame");
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("JSON");
    assert_eq!(reply["id"], 1);
    assert!(reply["result"]["protocolVersion"].is_number(), "{reply}");

    tokio::time::timeout(Duration::from_secs(10), backend.shutdown())
        .await
        .expect("shutdown within 10s");
}

/// Shutdown ends the thread and the frame stream, and later sends fail.
#[tokio::test]
async fn shutdown_releases_the_backend() {
    let home = tempfile::tempdir().expect("data home");
    let (mut backend, output) =
        BackendThread::start(config_in(&home)).await.expect("start");

    tokio::time::timeout(Duration::from_secs(5), backend.shutdown())
        .await
        .expect("the thread exits within 5s");

    assert!(backend.send(INITIALIZE.to_owned()).is_err());
    let frames: Vec<String> = output.frames.collect().await;
    assert!(frames.is_empty(), "{frames:?}");
    assert_eq!(output.ended.await.expect("end reported"), None);
}

/// The quit path blocks until the thread has stopped, within its bound.
#[tokio::test]
async fn shutdown_blocking_waits_for_the_thread() {
    let home = tempfile::tempdir().expect("data home");
    let (mut backend, output) =
        BackendThread::start(config_in(&home)).await.expect("start");

    let stopped = backend.shutdown_blocking(Duration::from_secs(10));

    assert!(stopped, "the thread stops within the bound");
    assert!(backend.send(INITIALIZE.to_owned()).is_err());
    assert_eq!(output.ended.await.expect("end reported"), None);
}

/// A configuration the application rejects fails `start` with its reason.
#[tokio::test]
async fn startup_failure_returns_the_reason() {
    let home = tempfile::tempdir().expect("data home");
    let config = ConfigHandle::from_config(AppConfig {
        session_persistence: SessionPersistenceConfig {
            data_home: Some(home.path().to_str().expect("utf8").to_owned()),
        },
        extensions: ExtensionsConfig {
            enabled: vec![
                protocol::ExtensionId::try_from("hook-examples")
                    .expect("extension id"),
            ],
        },
        ..AppConfig::default()
    });

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        BackendThread::start(config),
    )
    .await
    .expect("start must not hang");

    match result {
        Err(message) => assert!(message.contains("hook-examples"), "{message}"),
        Ok(_) => panic!("an uncompiled extension must fail startup"),
    }
}
