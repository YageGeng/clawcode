//! ACP over string frames (`AcpServerFactory::serve_lines`), the transport the
//! NSH desktop extension uses to bridge frames into TSX.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acp::AcpServerFactory;
use extension::StaticExtensionFactory;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{ModelProfile, ModelRequest};
use store::{JsonlStoreFactory, SystemClock};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Model factory for transport tests that never run inference.
struct UnusedModelFactory;

impl ModelFactory for UnusedModelFactory {
    /// Creates a model handle that rejects any inference.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(UnusedModel {
            profile: ModelProfile::builder()
                .provider_id("integration".to_string())
                .model_id("unused".to_string())
                .display_name("Unused integration model".to_string())
                .context_tokens(32_768)
                .max_output_tokens(1_024)
                .build(),
        }))
    }
}

/// Model handle required by Kernel construction but never used here.
struct UnusedModel {
    profile: ModelProfile,
}

#[async_trait::async_trait]
impl Model for UnusedModel {
    /// Returns the deterministic profile the Kernel requires.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Rejects readiness checks: transport tests must not reach a model.
    async fn preflight(&self) -> Result<(), ModelError> {
        Err(ModelError::Unavailable(
            "lines tests must not preflight".into(),
        ))
    }

    /// Rejects inference: transport tests must not reach a model.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Err(ModelError::Unavailable(
            "lines tests must not stream".into(),
        ))
    }
}

/// Builds a production Kernel over `root` with deterministic local dependencies.
fn lines_kernel(root: &Path) -> Arc<Kernel> {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(UnusedModelFactory))
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
            .expect("lines kernel"),
    )
}

/// Sends `request` over `serve_lines` and returns the first frame the server writes.
async fn first_reply(request: serde_json::Value) -> String {
    let state = tempfile::tempdir().expect("state");
    let factory = AcpServerFactory::new(
        lines_kernel(state.path()),
        NonZeroUsize::new(128).expect("positive batch size"),
    );
    let (outgoing, mut written) = mpsc::unbounded::<String>();
    let (mut send, incoming) = mpsc::unbounded::<std::io::Result<String>>();
    let outgoing = outgoing.sink_map_err(std::io::Error::other);
    let server =
        tokio::spawn(
            async move { factory.serve_lines(outgoing, incoming).await },
        );
    send.send(Ok(request.to_string()))
        .await
        .expect("send request");
    let reply = tokio::time::timeout(Duration::from_secs(10), written.next())
        .await
        .expect("reply within 10s")
        .expect("server wrote a frame");
    // Ending the incoming stream ends the connection.
    drop(send);
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server ends after input closes")
        .expect("server task")
        .expect("serve_lines");
    reply
}

/// Builds an `initialize` request with the given JSON-RPC id.
fn initialize(id: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": { "protocolVersion": 2, "info": { "name": "lines-test", "version": "0.1.0" } }
    })
}

/// `initialize` written as one string frame gets its answer as one string frame.
#[tokio::test]
async fn serve_lines_answers_initialize() {
    let reply = first_reply(initialize(serde_json::json!(1))).await;
    let reply: serde_json::Value =
        serde_json::from_str(&reply).expect("reply is JSON");
    assert_eq!(reply["id"], 1);
    assert!(
        reply["result"]["protocolVersion"].is_number(),
        "unexpected reply: {reply}"
    );
}

/// Non-ASCII text and escaped newlines cross the frame boundary unchanged.
#[tokio::test]
async fn lines_round_trip_unicode_and_escaped_newlines() {
    let id = "初始化 🌍\nline two";
    let reply = first_reply(initialize(serde_json::json!(id))).await;
    assert!(
        !reply.contains('\n'),
        "a frame holds no raw newline: {reply:?}"
    );
    let reply: serde_json::Value =
        serde_json::from_str(&reply).expect("reply is JSON");
    assert_eq!(reply["id"], id);
}
