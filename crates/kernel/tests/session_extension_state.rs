use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionError,
    ExtensionModule, ExtensionRegistrar, StaticExtensionFactory,
};
use futures::{Stream, stream};
use kernel::{
    KernelFactory, Model, ModelCatalog, ModelError, ModelFactory,
    NanoidIdGenerator,
};
use protocol::{
    ExtensionCommandDefinition, ExtensionDescriptor, ExtensionId,
    ExtensionSnapshot, ModelFinal, ModelProfile, ModelRequest,
    ModelStreamEvent, ModelUsage, SessionId, StaticExtensionRegistration,
    StopReason, ThinkingLevel,
};
use store::{JsonlStoreFactory, SessionCreateOptions, SystemClock};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Deterministic local model with one configurable profile.
struct StateModel(ModelProfile);

#[async_trait]
impl Model for StateModel {
    /// Returns the configured profile.
    fn profile(&self) -> &ModelProfile {
        &self.0
    }

    /// Confirms that no external provider is required.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Returns one terminal response when the model is invoked unexpectedly.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>,
        >,
        ModelError,
    > {
        Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
            ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(0)
                    .output_tokens(0)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(0)
                    .build(),
            },
        ))])))
    }
}

/// Creates a two-model catalog so session-local switching can be observed.
struct StateModelFactory;

impl StateModelFactory {
    /// Builds one fixture model profile.
    fn model(id: &str) -> Arc<dyn Model> {
        Arc::new(StateModel(
            ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id(id.to_string())
                .display_name(id.to_string())
                .context_tokens(128_000)
                .max_output_tokens(8_000)
                .build(),
        ))
    }
}

impl ModelFactory for StateModelFactory {
    /// Creates the configured active model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Self::model("primary"))
    }

    /// Returns both fixture models with `primary` selected initially.
    fn create_catalog(
        &self,
        _registration: &StaticExtensionRegistration,
    ) -> Result<ModelCatalog, ModelError> {
        ModelCatalog::new(
            "fixture",
            "primary",
            vec![Self::model("primary"), Self::model("secondary")],
        )
    }
}

/// Command that mutates or captures the current session extension state.
struct StateCommand {
    snapshots: Arc<Mutex<Vec<ExtensionSnapshot>>>,
}

#[async_trait]
impl ExtensionCommandHandler for StateCommand {
    /// Applies requested state changes or records a fresh snapshot.
    async fn handle(
        &self,
        _arguments: &str,
        parameters: &serde_json::Value,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        match parameters.get("action").and_then(|value| value.as_str()) {
            Some("set") => {
                context
                    .event
                    .set_model("fixture", "secondary")
                    .await
                    .map_err(|error| {
                        ExtensionError::Handler(error.to_string())
                    })?;
                context
                    .event
                    .set_thinking_level(ThinkingLevel::High)
                    .await
                    .map_err(|error| {
                        ExtensionError::Handler(error.to_string())
                    })?;
                context
                    .event
                    .set_active_tools(vec!["read".to_string()])
                    .await
                    .map_err(|error| ExtensionError::Handler(error.to_string()))
            }
            Some("inspect") => {
                let snapshot = context.event.snapshot().map_err(|error| {
                    ExtensionError::Handler(error.to_string())
                })?;
                self.snapshots.lock().expect("snapshot lock").push(snapshot);
                Ok(())
            }
            _ => Err(ExtensionError::Handler("unknown action".to_string())),
        }
    }
}

/// Per-session module that shares only the external assertion recorder.
struct StateModule {
    snapshots: Arc<Mutex<Vec<ExtensionSnapshot>>>,
}

impl ExtensionModule for StateModule {
    /// Returns the stable state extension descriptor.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("state").expect("extension id"),
            name: "State".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the state command for one session runtime.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            ExtensionCommandDefinition {
                name: "state".to_string(),
                description: None,
            },
            StateCommand {
                snapshots: Arc::clone(&self.snapshots),
            },
        )
    }
}

/// Session model, thinking, and active-tool selections survive runtime recreation.
#[tokio::test]
async fn extension_session_state_is_persisted_and_restored() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let extension_snapshots = Arc::clone(&snapshots);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StateModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::new(SystemClock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(StateModule {
                    snapshots: Arc::clone(&extension_snapshots),
                }) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-state").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
        .invoke_extension_command(
            &session_id,
            "state".to_string(),
            serde_json::json!({ "action": "set" }),
        )
        .await
        .expect("set session state");
    kernel
        .close_session(&session_id)
        .await
        .expect("close session");
    kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("resume session");
    kernel
        .invoke_extension_command(
            &session_id,
            "state".to_string(),
            serde_json::json!({ "action": "inspect" }),
        )
        .await
        .expect("inspect session state");

    let snapshots = snapshots.lock().expect("snapshot lock");
    let snapshot = snapshots.last().expect("captured snapshot");
    assert_eq!(
        snapshot
            .active_model
            .as_ref()
            .expect("active model")
            .model_id,
        "secondary"
    );
    assert_eq!(snapshot.thinking_level, ThinkingLevel::High);
    assert_eq!(snapshot.active_tools, vec!["read"]);
    assert_eq!(snapshot.models.len(), 2);
    let kinds = snapshot
        .tree
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"model_change"));
    assert!(kinds.contains(&"thinking_level_change"));
    assert!(kinds.contains(&"active_tools_change"));
}
