use std::path::Path;
use std::sync::{Arc, Mutex};

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::Client;
use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use extension::StaticExtensionFactory;
use futures::stream;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent, ModelUsage,
    SessionId, StopReason,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Model that pauses one ACP Prompt until its client connection has closed.
struct DisconnectModel {
    profile: ModelProfile,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl Model for DisconnectModel {
    /// Returns the deterministic disconnect-test profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms the local fixture has no readiness dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Waits across disconnect before returning one durable assistant response.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::pin(stream::iter([
            Ok(ModelStreamEvent::TextDelta("detached complete".to_string())),
            Ok(ModelStreamEvent::Finished(ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: Some("stop".to_string()),
                usage: ModelUsage::builder()
                    .input_tokens(1)
                    .output_tokens(2)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(3)
                    .build(),
            })),
        ])))
    }
}

/// Supplies one shared disconnect model to the ACP integration test.
struct DisconnectModelFactory(Arc<DisconnectModel>);

impl ModelFactory for DisconnectModelFactory {
    /// Returns the shared deterministic model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }
}

/// Builds one production Kernel around the disconnect model.
fn disconnect_kernel(root: &Path, model: Arc<DisconnectModel>) -> Arc<Kernel> {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(DisconnectModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .build()
            .build()
            .expect("disconnect kernel"),
    )
}

/// ACP disconnect drops live projection only while the Kernel Run completes durably.
#[tokio::test]
async fn prompt_run_survives_client_disconnect() {
    let state = tempfile::tempdir().expect("store directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let model = Arc::new(DisconnectModel {
        profile: ModelProfile::builder()
            .provider_id("fixture".to_string())
            .model_id("disconnect".to_string())
            .display_name("Disconnect fixture".to_string())
            .context_tokens(8_192)
            .max_output_tokens(1_024)
            .build(),
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let kernel = disconnect_kernel(state.path(), Arc::clone(&model));
    let server =
        AcpServerFactory::new(Arc::clone(&kernel), Arc::new(NanoidIdGenerator))
            .component(AcpTransportKind::Stdio);
    let captured_session_id = Arc::new(Mutex::new(None::<SessionId>));
    let client_session_id = Arc::clone(&captured_session_id);
    let started = Arc::clone(&model.started);

    Client
        .v2()
        .connect_with(server, async move |connection| {
            connection
                .send_request(wire::InitializeRequest::new(
                    ProtocolVersion::V2,
                    wire::Implementation::new("disconnect-test", "0.1.0"),
                ))
                .block_task()
                .await?;
            let created = connection
                .send_request(wire::NewSessionRequest::new(
                    workspace.path().to_path_buf(),
                ))
                .block_task()
                .await?;
            let session_id = SessionId::try_from(
                created.session_id.to_string(),
            )
            .map_err(agent_client_protocol::Error::into_internal_error)?;
            *client_session_id.lock().map_err(|_poison_error| {
                agent_client_protocol::Error::into_internal_error(
                    std::io::Error::other("session id lock poisoned"),
                )
            })? = Some(session_id);
            connection
                .send_request(wire::PromptRequest::new(
                    created.session_id,
                    vec![wire::ContentBlock::Text(wire::TextContent::new(
                        "wait across disconnect",
                    ))],
                ))
                .block_task()
                .await?;
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                started.notified(),
            )
            .await
            .map_err(agent_client_protocol::Error::into_internal_error)?;
            Ok(())
        })
        .await
        .expect("ACP connection");

    let session_id = captured_session_id
        .lock()
        .expect("session id lock")
        .clone()
        .expect("captured Session id");
    assert!(
        kernel
            .session_runtime(&session_id)
            .expect("runtime after disconnect")
            .running
    );

    model.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !kernel
                .session_runtime(&session_id)
                .expect("runtime after release")
                .running
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached Run settles");

    assert!(
        kernel
            .session_messages(&session_id)
            .expect("persisted messages")
            .iter()
            .any(|message| {
                message.content.text_content().as_deref()
                    == Some("detached complete")
            })
    );
}
