use std::path::Path;
use std::sync::Arc;

use acp::AcpServerFactory;
use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use agent_client_protocol::{
    Client, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, UntypedMessage,
};
use extension::StaticExtensionFactory;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use protocol::{AcpExtensionMethod, ModelProfile, ModelRequest, SessionId};
use store::{JsonlStoreFactory, SystemClock};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

const SESSION_NEW_METHOD: &str = "session/new";
const SESSION_LIST_METHOD: &str = "session/list";
const SESSION_DELETE_METHOD: &str = "session/delete";

/// Model factory that proves extension validation does not require inference.
struct UnusedModelFactory;

impl ModelFactory for UnusedModelFactory {
    /// Creates a valid model handle that rejects any accidental inference.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(UnusedModel {
            profile: ModelProfile::builder()
                .provider_id("integration".to_string())
                .model_id("unused".to_string())
                .display_name("Unused integration model".to_string())
                .context_tokens(8_192)
                .max_output_tokens(1_024)
                .build(),
        }))
    }
}

/// Model handle required by Kernel construction but never used by these tests.
struct UnusedModel {
    profile: ModelProfile,
}

#[async_trait::async_trait]
impl Model for UnusedModel {
    /// Returns the deterministic profile required by the Kernel contract.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Rejects accidental provider readiness checks in transport validation tests.
    async fn preflight(&self) -> Result<(), ModelError> {
        Err(ModelError::Unavailable(
            "extension integration tests must not preflight a model"
                .to_string(),
        ))
    }

    /// Rejects accidental inference in transport validation tests.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Err(ModelError::Unavailable(
            "extension integration tests must not invoke a model".to_string(),
        ))
    }
}

/// JSON request used to exercise public ACP methods without exposing private parameter types.
#[derive(Debug, Clone)]
struct IntegrationRequest {
    method: &'static str,
    parameters: serde_json::Value,
}

impl IntegrationRequest {
    /// Creates one method-preserving request for the public ACP connection.
    fn new(method: &'static str, parameters: serde_json::Value) -> Self {
        Self { method, parameters }
    }
}

impl JsonRpcMessage for IntegrationRequest {
    /// Restricts inbound parsing to the methods exercised by this test boundary.
    fn matches_method(method: &str) -> bool {
        method == SESSION_NEW_METHOD
            || method == SESSION_LIST_METHOD
            || method == SESSION_DELETE_METHOD
            || method == AcpExtensionMethod::Compact.as_str()
            || method == AcpExtensionMethod::UserBash.as_str()
    }

    /// Returns the static ACP method associated with this request.
    fn method(&self) -> &'static str {
        self.method
    }

    /// Serializes the request through the same untyped ACP envelope used by clients.
    fn to_untyped_message(
        &self,
    ) -> Result<UntypedMessage, agent_client_protocol::Error> {
        UntypedMessage::new(self.method, &self.parameters)
    }

    /// Parses a supported integration request without exposing production internals.
    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Result<Self, agent_client_protocol::Error> {
        let method = match method {
            SESSION_NEW_METHOD => SESSION_NEW_METHOD,
            SESSION_LIST_METHOD => SESSION_LIST_METHOD,
            SESSION_DELETE_METHOD => SESSION_DELETE_METHOD,
            method if method == AcpExtensionMethod::Compact.as_str() => {
                AcpExtensionMethod::Compact.as_str()
            }
            method if method == AcpExtensionMethod::UserBash.as_str() => {
                AcpExtensionMethod::UserBash.as_str()
            }
            _ => {
                return Err(agent_client_protocol::Error::method_not_found());
            }
        };
        let parameters = serde_json::to_value(params)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        Ok(Self { method, parameters })
    }
}

impl JsonRpcRequest for IntegrationRequest {
    type Response = IntegrationResponse;
}

/// Opaque response that preserves the public JSON result for assertions.
#[derive(Debug, Clone)]
struct IntegrationResponse(serde_json::Value);

impl JsonRpcResponse for IntegrationResponse {
    /// Returns the response JSON without applying a private schema.
    fn into_json(
        self,
        _method: &str,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        Ok(self.0)
    }

    /// Captures the public ACP response JSON for integration assertions.
    fn from_value(
        _method: &str,
        value: serde_json::Value,
    ) -> Result<Self, agent_client_protocol::Error> {
        Ok(Self(value))
    }
}

/// Builds a production ACP component with local deterministic dependencies.
fn integration_kernel(root: &Path) -> Arc<Kernel> {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(UnusedModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root,
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("integration kernel");
    Arc::new(kernel)
}

/// Sends one request through the public ACP v2 component after initialization.
async fn send_request(
    kernel: Arc<Kernel>,
    request: IntegrationRequest,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    Client
        .v2()
        .connect_with(
            AcpServerFactory::new(kernel).component(),
            async move |connection| {
                connection
                    .send_request(wire::InitializeRequest::new(
                        ProtocolVersion::V2,
                        wire::Implementation::new(
                            "acp-integration-test",
                            "0.1.0",
                        ),
                    ))
                    .block_task()
                    .await?;
                connection
                    .send_request(request)
                    .block_task()
                    .await
                    .map(|response| response.0)
            },
        )
        .await
}

/// Session creation accepts only absolute existing directories through public ACP.
#[tokio::test]
async fn working_directory_validation_is_enforced_by_session_new() {
    let root = tempfile::tempdir().expect("temporary directory");
    let state = tempfile::tempdir().expect("store directory");
    let kernel = integration_kernel(state.path());
    let accepted = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            SESSION_NEW_METHOD,
            serde_json::json!({ "cwd": root.path() }),
        ),
    )
    .await
    .expect("existing absolute directory");
    assert!(accepted["sessionId"].as_str().is_some());

    for invalid in [
        std::path::PathBuf::from("relative/path"),
        root.path().join("missing"),
    ] {
        let error = send_request(
            Arc::clone(&kernel),
            IntegrationRequest::new(
                SESSION_NEW_METHOD,
                serde_json::json!({ "cwd": invalid }),
            ),
        )
        .await
        .expect_err("invalid directory");
        assert_eq!(error.code, agent_client_protocol::ErrorCode::InvalidParams);
    }

    let file = root.path().join("file.txt");
    std::fs::write(&file, "content").expect("write file");
    let error = send_request(
        kernel,
        IntegrationRequest::new(
            SESSION_NEW_METHOD,
            serde_json::json!({ "cwd": file }),
        ),
    )
    .await
    .expect_err("file path");
    assert_eq!(error.code, agent_client_protocol::ErrorCode::InvalidParams);
}

/// Compact rejects caller-supplied summaries through the public extension method.
#[tokio::test]
async fn compact_rejects_client_supplied_summary() {
    let root = tempfile::tempdir().expect("temporary directory");
    let state = tempfile::tempdir().expect("store directory");
    let kernel = integration_kernel(state.path());
    let session = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            SESSION_NEW_METHOD,
            serde_json::json!({ "cwd": root.path() }),
        ),
    )
    .await
    .expect("create session");
    let session_id = session["sessionId"].as_str().expect("session id");

    let error = send_request(
        kernel,
        IntegrationRequest::new(
            AcpExtensionMethod::Compact.as_str(),
            serde_json::json!({
                "sessionId": session_id,
                "summary": "client supplied"
            }),
        ),
    )
    .await
    .expect_err("client summary must be rejected");
    assert_eq!(error.code, agent_client_protocol::ErrorCode::InvalidParams);
}

/// ACP v2 advertises native deletion and removes sessions from subsequent lists.
#[tokio::test]
async fn native_session_delete_is_advertised_and_idempotent() {
    let workspace = tempfile::tempdir().expect("workspace directory");
    let state = tempfile::tempdir().expect("store directory");
    let kernel = integration_kernel(state.path());

    Client
        .v2()
        .connect_with(
            AcpServerFactory::new(kernel).component(),
            async move |connection| {
                let initialized = connection
                    .send_request(wire::InitializeRequest::new(
                        ProtocolVersion::V2,
                        wire::Implementation::new(
                            "acp-integration-test",
                            "0.1.0",
                        ),
                    ))
                    .block_task()
                    .await?;
                assert!(
                    initialized
                        .capabilities
                        .session
                        .and_then(|session| session.delete)
                        .is_some()
                );
                let created = connection
                    .send_request(IntegrationRequest::new(
                        SESSION_NEW_METHOD,
                        serde_json::json!({ "cwd": workspace.path() }),
                    ))
                    .block_task()
                    .await?;
                let session_id = created.0["sessionId"]
                    .as_str()
                    .expect("created session id");

                for _attempt in 0..2 {
                    connection
                        .send_request(IntegrationRequest::new(
                            SESSION_DELETE_METHOD,
                            serde_json::json!({ "sessionId": session_id }),
                        ))
                        .block_task()
                        .await?;
                }
                let listed = connection
                    .send_request(IntegrationRequest::new(
                        SESSION_LIST_METHOD,
                        serde_json::json!({}),
                    ))
                    .block_task()
                    .await?;
                assert_eq!(listed.0["sessions"], serde_json::json!([]));
                Ok(())
            },
        )
        .await
        .expect("native delete request");
}

/// ACP exposes server-side bash without negotiating client terminal callbacks.
#[tokio::test]
async fn user_bash_extension_executes_and_persists_on_the_server() {
    let workspace = tempfile::tempdir().expect("workspace directory");
    let state = tempfile::tempdir().expect("store directory");
    let kernel = integration_kernel(state.path());
    let session = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            SESSION_NEW_METHOD,
            serde_json::json!({ "cwd": workspace.path() }),
        ),
    )
    .await
    .expect("create session");
    let session_id = session["sessionId"].as_str().expect("session id");

    let result = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            AcpExtensionMethod::UserBash.as_str(),
            serde_json::json!({
                "sessionId": session_id,
                "command": "printf 'acp bash'",
                "excludeFromContext": true
            }),
        ),
    )
    .await
    .expect("execute server bash");

    assert_eq!(result["output"], "acp bash");
    assert_eq!(result["exitCode"], 0);
    assert_eq!(result["cancelled"], false);
    assert_eq!(result["truncated"], false);
    let session_id = SessionId::try_from(session_id).expect("session id");
    let transcript = kernel
        .session_transcript(&session_id)
        .expect("persisted transcript");
    assert!(matches!(
        &transcript[0].content,
        protocol::MessageContent::BashExecution { bash }
            if bash.exclude_from_context && bash.result.output == "acp bash"
    ));
}
