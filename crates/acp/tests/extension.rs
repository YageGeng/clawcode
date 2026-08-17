use std::path::Path;
use std::sync::Arc;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use agent_client_protocol::{
    Client, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, UntypedMessage,
};
use extension::StaticExtensionFactory;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AcpExtensionMethod, AgentMessage, CompactionData, CompactionDetails,
    CompactionReason, ContentBlock, EntryId, MessageContent, MessageId,
    MessageIdentity, MessageTiming, ModelProfile, ModelRequest,
    ProductIdentity, RunId, SessionId, TimestampMs, TurnId,
};
use skill::FilesystemSkillFactory;
use store::{
    EntryKind, JsonlStoreFactory, NewEntry, SessionCreateOptions, StoreFactory,
    SystemClock,
};
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
            || method == AcpExtensionMethod::Fork.as_str()
            || method == AcpExtensionMethod::InvokeSkill.as_str()
            || method == AcpExtensionMethod::SkillList.as_str()
            || method == AcpExtensionMethod::McpStatus.as_str()
            || method == AcpExtensionMethod::McpElicitationList.as_str()
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
            method if method == AcpExtensionMethod::Fork.as_str() => {
                AcpExtensionMethod::Fork.as_str()
            }
            method if method == AcpExtensionMethod::InvokeSkill.as_str() => {
                AcpExtensionMethod::InvokeSkill.as_str()
            }
            method if method == AcpExtensionMethod::SkillList.as_str() => {
                AcpExtensionMethod::SkillList.as_str()
            }
            method if method == AcpExtensionMethod::McpStatus.as_str() => {
                AcpExtensionMethod::McpStatus.as_str()
            }
            method
                if method
                    == AcpExtensionMethod::McpElicitationList.as_str() =>
            {
                AcpExtensionMethod::McpElicitationList.as_str()
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

/// ACP exposes the latest MCP snapshot and rejects extra status parameters.
#[tokio::test]
async fn mcp_status_is_session_scoped_and_strict() {
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

    let snapshot = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            AcpExtensionMethod::McpStatus.as_str(),
            serde_json::json!({ "sessionId": session_id }),
        ),
    )
    .await
    .expect("MCP status");
    assert_eq!(snapshot["revision"], 0);
    assert_eq!(snapshot["servers"], serde_json::json!([]));
    assert_eq!(snapshot["catalog"]["tools"], serde_json::json!([]));

    let error = send_request(
        kernel,
        IntegrationRequest::new(
            AcpExtensionMethod::McpStatus.as_str(),
            serde_json::json!({
                "sessionId": session_id,
                "unexpected": true
            }),
        ),
    )
    .await
    .expect_err("unknown status field");
    assert_eq!(error.code, agent_client_protocol::ErrorCode::InvalidParams);
}

/// ACP exposes a strict Session-scoped snapshot of transient MCP elicitations.
#[tokio::test]
async fn mcp_elicitation_list_is_session_scoped_and_strict() {
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

    let snapshot = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            AcpExtensionMethod::McpElicitationList.as_str(),
            serde_json::json!({ "sessionId": session_id }),
        ),
    )
    .await
    .expect("MCP elicitation list");
    assert_eq!(snapshot["sessionId"], session_id);
    assert_eq!(snapshot["requests"], serde_json::json!([]));

    let error = send_request(
        kernel,
        IntegrationRequest::new(
            AcpExtensionMethod::McpElicitationList.as_str(),
            serde_json::json!({
                "sessionId": session_id,
                "unexpected": true
            }),
        ),
    )
    .await
    .expect_err("unknown elicitation list field");
    assert_eq!(error.code, agent_client_protocol::ErrorCode::InvalidParams);
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
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.join("config"),
            protocol::PromptPolicy::default(),
        )))
        .skill_factory(Some(Arc::new(
            FilesystemSkillFactory::builder()
                .global_root(root.join("config"))
                .user_home(root.join("home"))
                .build(),
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
            AcpServerFactory::new(kernel, Arc::new(NanoidIdGenerator))
                .component(AcpTransportKind::Stdio),
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
            AcpServerFactory::new(kernel, Arc::new(NanoidIdGenerator))
                .component(AcpTransportKind::Stdio),
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

/// ACP Skill methods require a Session and resolve its frozen catalog.
#[tokio::test]
async fn skill_extensions_are_session_scoped() {
    let workspace = tempfile::tempdir().expect("workspace directory");
    let state = tempfile::tempdir().expect("store directory");
    let skill = state.path().join("config/skills/review/SKILL.md");
    let project_skill = workspace.path().join(".pi/skills/review/SKILL.md");
    std::fs::create_dir_all(skill.parent().expect("Skill parent"))
        .expect("create Skill parent");
    std::fs::create_dir_all(project_skill.parent().expect("Skill parent"))
        .expect("create project Skill parent");
    std::fs::write(
        &skill,
        "---\nname: review\ndescription: Review code\n---\nREVIEW BODY\n",
    )
    .expect("write Skill");
    std::fs::write(
        &project_skill,
        "---\nname: review\ndescription: Project review\n---\nPROJECT REVIEW BODY\n",
    )
    .expect("write project Skill");
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

    let listed = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            AcpExtensionMethod::SkillList.as_str(),
            serde_json::json!({ "sessionId": session_id }),
        ),
    )
    .await
    .expect("list Session Skills");
    assert_eq!(listed["skills"][0]["name"], "review");
    assert_eq!(listed["skills"][0]["source"]["scope"], "project");
    assert!(listed["skills"][0].get("content").is_none());
    assert_eq!(listed["diagnostics"][0]["code"], "name_collision");
    assert_eq!(
        listed["diagnostics"][0]["collision"]["winnerPath"],
        project_skill.to_string_lossy().as_ref()
    );
    assert_eq!(
        listed["diagnostics"][0]["collision"]["loserPath"],
        skill.to_string_lossy().as_ref()
    );

    let invoked = send_request(
        Arc::clone(&kernel),
        IntegrationRequest::new(
            AcpExtensionMethod::InvokeSkill.as_str(),
            serde_json::json!({
                "sessionId": session_id,
                "name": "review"
            }),
        ),
    )
    .await
    .expect("invoke Session Skill");
    assert_eq!(invoked["name"], "review");
    assert!(
        invoked["content"]
            .as_str()
            .is_some_and(|content| { content.contains("PROJECT REVIEW BODY") })
    );

    let error = send_request(
        kernel,
        IntegrationRequest::new(
            AcpExtensionMethod::SkillList.as_str(),
            serde_json::json!({}),
        ),
    )
    .await
    .expect_err("missing Session id");
    assert_eq!(error.code, agent_client_protocol::ErrorCode::InvalidParams);
}

/// New, Resume, and Fork each publish a complete native command snapshot.
#[tokio::test]
async fn session_activation_publishes_available_commands() {
    let workspace = tempfile::tempdir().expect("workspace directory");
    let state = tempfile::tempdir().expect("store directory");
    let prompt = state.path().join("config/prompts/review.md");
    std::fs::create_dir_all(prompt.parent().expect("Prompt parent"))
        .expect("create Prompt parent");
    std::fs::write(
        prompt,
        "---\ndescription: Review changes\nargument-hint: <path>\n---\nreview\n",
    )
    .expect("write Prompt Template");
    let kernel = integration_kernel(state.path());
    let server =
        AcpServerFactory::new(Arc::clone(&kernel), Arc::new(NanoidIdGenerator))
            .component(AcpTransportKind::Stdio);
    let (updates_tx, mut updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = Client.v2().on_receive_notification(
        async move |notification: wire::UpdateSessionNotification,
                    _connection| {
            updates_tx.send(notification).map_err(|_error| {
                agent_client_protocol::Error::into_internal_error(
                    std::io::Error::other("notification receiver closed"),
                )
            })?;
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );

    client
        .connect_with(server, async move |connection| {
            connection
                .send_request(wire::InitializeRequest::new(
                    ProtocolVersion::V2,
                    wire::Implementation::new("commands-test", "0.1.0"),
                ))
                .block_task()
                .await?;
            let created = connection
                .send_request(wire::NewSessionRequest::new(
                    workspace.path().to_path_buf(),
                ))
                .block_task()
                .await?;
            let session_id =
                SessionId::try_from(created.session_id.to_string())
                    .expect("Session id");
            let first = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                updates_rx.recv(),
            )
            .await
            .expect("New command snapshot")
            .expect("New notification");
            assert_eq!(first.session_id.to_string(), session_id.as_str());
            let wire::SessionUpdate::AvailableCommandsUpdate(commands) =
                first.update
            else {
                panic!("Available Commands update expected after New");
            };
            let review = commands
                .available_commands
                .iter()
                .find(|command| command.name == "review")
                .expect("Review Prompt Template command");
            assert_eq!(
                review.input.as_ref().and_then(|input| match input {
                    wire::AvailableCommandInput::Text(input) => {
                        Some(input.hint.as_str())
                    }
                    _ => None,
                }),
                Some("<path>")
            );

            kernel
                .close_session(&session_id)
                .await
                .map_err(agent_client_protocol::Error::into_internal_error)?;
            connection
                .send_request(wire::ResumeSessionRequest::new(
                    session_id.to_string(),
                    workspace.path().to_path_buf(),
                ))
                .block_task()
                .await?;
            let resumed = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                updates_rx.recv(),
            )
            .await
            .expect("Resume command snapshot")
            .expect("Resume notification");
            assert_eq!(resumed.session_id.to_string(), session_id.as_str());
            assert!(matches!(
                resumed.update,
                wire::SessionUpdate::AvailableCommandsUpdate(_)
            ));

            connection
                .send_request(IntegrationRequest::new(
                    AcpExtensionMethod::UserBash.as_str(),
                    serde_json::json!({
                        "sessionId": session_id,
                        "command": "printf 'fork source'",
                        "excludeFromContext": true
                    }),
                ))
                .block_task()
                .await?;
            let entry_id = kernel
                .session_tree(&session_id)
                .map_err(agent_client_protocol::Error::into_internal_error)?
                .entries
                .into_iter()
                .find(|entry| entry.kind == "message")
                .expect("fork source entry")
                .entry_id;
            let fork_id = "session-command-fork";
            connection
                .send_request(IntegrationRequest::new(
                    AcpExtensionMethod::Fork.as_str(),
                    serde_json::json!({
                        "sessionId": session_id,
                        "entryId": entry_id,
                        "cwd": workspace.path(),
                        "newSessionId": fork_id
                    }),
                ))
                .block_task()
                .await?;
            loop {
                let notification = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    updates_rx.recv(),
                )
                .await
                .expect("Fork command snapshot")
                .expect("Fork notification");
                if matches!(
                    notification.update,
                    wire::SessionUpdate::AvailableCommandsUpdate(_)
                ) {
                    assert_eq!(notification.session_id.to_string(), fork_id);
                    break;
                }
            }
            Ok(())
        })
        .await
        .expect("Available Commands lifecycle");
}

/// ACP executes a Builtin command as correlated messages and replays both in order.
#[tokio::test]
async fn slash_command_messages_round_trip_through_acp_replay() {
    let workspace = tempfile::tempdir().expect("workspace directory");
    let state = tempfile::tempdir().expect("store directory");
    let kernel = integration_kernel(state.path());
    let server =
        AcpServerFactory::new(Arc::clone(&kernel), Arc::new(NanoidIdGenerator))
            .component(AcpTransportKind::Stdio);
    let (updates_tx, mut updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = Client.v2().on_receive_notification(
        async move |notification: wire::UpdateSessionNotification,
                    _connection| {
            updates_tx.send(notification).map_err(|_error| {
                agent_client_protocol::Error::into_internal_error(
                    std::io::Error::other("notification receiver closed"),
                )
            })?;
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );

    client
        .connect_with(server, async move |connection| {
            connection
                .send_request(wire::InitializeRequest::new(
                    ProtocolVersion::V2,
                    wire::Implementation::new("slash-replay-test", "0.1.0"),
                ))
                .block_task()
                .await?;
            let created = connection
                .send_request(wire::NewSessionRequest::new(
                    workspace.path().to_path_buf(),
                ))
                .block_task()
                .await?;
            let session_id =
                SessionId::try_from(created.session_id.to_string())
                    .expect("Session id");
            let initial = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                updates_rx.recv(),
            )
            .await
            .expect("initial command snapshot")
            .expect("initial notification");
            assert!(matches!(
                initial.update,
                wire::SessionUpdate::AvailableCommandsUpdate(_)
            ));

            connection
                .send_request(wire::PromptRequest::new(
                    created.session_id.clone(),
                    vec![wire::ContentBlock::Text(wire::TextContent::new(
                        "/session",
                    ))],
                ))
                .block_task()
                .await?;
            let mut live_messages = Vec::new();
            loop {
                let notification = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    updates_rx.recv(),
                )
                .await
                .expect("live command update")
                .expect("live notification");
                let is_idle = matches!(
                    notification.update,
                    wire::SessionUpdate::StateUpdate(
                        wire::StateUpdate::Idle(_)
                    )
                );
                if matches!(
                    notification.update,
                    wire::SessionUpdate::UserMessage(_)
                        | wire::SessionUpdate::AgentMessage(_)
                ) {
                    live_messages.push(
                        serde_json::to_value(&notification.update)
                            .expect("serialize live message update"),
                    );
                }
                if is_idle {
                    break;
                }
            }
            assert_eq!(live_messages.len(), 2);
            assert_eq!(live_messages[0]["sessionUpdate"], "user_message");
            assert_eq!(live_messages[1]["sessionUpdate"], "agent_message");

            kernel
                .close_session(&session_id)
                .await
                .map_err(agent_client_protocol::Error::into_internal_error)?;
            connection
                .send_request(
                    wire::ResumeSessionRequest::new(
                        created.session_id,
                        workspace.path().to_path_buf(),
                    )
                    .replay_from(wire::ReplayFrom::from(
                        wire::ReplayFromStart::new(),
                    )),
                )
                .block_task()
                .await?;

            let mut replayed_messages = Vec::new();
            loop {
                let notification = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    updates_rx.recv(),
                )
                .await
                .expect("replayed command update")
                .expect("replayed notification");
                if matches!(
                    notification.update,
                    wire::SessionUpdate::AvailableCommandsUpdate(_)
                ) {
                    break;
                }
                if matches!(
                    notification.update,
                    wire::SessionUpdate::UserMessage(_)
                        | wire::SessionUpdate::AgentMessage(_)
                ) {
                    replayed_messages.push(
                        serde_json::to_value(&notification.update)
                            .expect("serialize replayed message update"),
                    );
                }
            }
            assert_eq!(replayed_messages.len(), 2);
            for (live, replayed) in
                live_messages.iter().zip(&replayed_messages)
            {
                assert_eq!(live["sessionUpdate"], replayed["sessionUpdate"]);
                assert_eq!(live["messageId"], replayed["messageId"]);
                assert_eq!(
                    live["content"][0]["type"],
                    replayed["content"][0]["type"]
                );
                assert_eq!(
                    live["content"][0]["text"],
                    replayed["content"][0]["text"]
                );
                assert_eq!(
                    live["_meta"][protocol::ProductIdentity::ACP_NAMESPACE]
                        ["turnId"],
                    replayed["_meta"]
                        [protocol::ProductIdentity::ACP_NAMESPACE]["turnId"]
                );
                assert_eq!(
                    live["_meta"][protocol::ProductIdentity::ACP_NAMESPACE]
                        [protocol::ProductIdentity::ACP_SLASH_COMMAND_METADATA],
                    replayed["_meta"]
                        [protocol::ProductIdentity::ACP_NAMESPACE]
                        [protocol::ProductIdentity::ACP_SLASH_COMMAND_METADATA]
                );
            }
            Ok(())
        })
        .await
        .expect("Slash Command ACP replay lifecycle");
}

/// ACP resume replays a durable compaction card between its neighboring messages.
#[tokio::test]
async fn compaction_card_replays_in_persisted_branch_order() {
    let workspace = tempfile::tempdir().expect("workspace directory");
    let state = tempfile::tempdir().expect("store directory");
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let store_factory = JsonlStoreFactory::new(state.path(), clock);
    let session_id =
        SessionId::try_from("session-compaction-replay").expect("session id");
    let mut store = store_factory
        .create(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: workspace.path().to_path_buf(),
            parent_session_id: None,
        })
        .expect("create persisted session");
    let first_timestamp = TimestampMs::from(1_000);
    let first = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-before")
                .expect("message id"),
            turn_id: TurnId::try_from("turn-before").expect("turn id"),
        },
        timing: MessageTiming::try_from((
            first_timestamp,
            first_timestamp,
            first_timestamp,
        ))
        .expect("message timing"),
        content: MessageContent::User {
            blocks: vec![ContentBlock::Text {
                text: "before compaction".to_string(),
            }],
        },
    };
    store
        .append_entry(
            &protocol::LaneId::try_from("main").expect("lane id"),
            NewEntry {
                id: EntryId::try_from("entry-before").expect("entry id"),
                kind: EntryKind::Message,
                payload: serde_json::to_value(first)
                    .expect("serialize first message"),
            },
        )
        .expect("append first message");
    let compaction_entry_id =
        EntryId::try_from("entry-compaction").expect("entry id");
    store
        .append_entry(
            &protocol::LaneId::try_from("main").expect("lane id"),
            NewEntry {
                id: compaction_entry_id.clone(),
                kind: EntryKind::Compaction,
                payload: serde_json::to_value(
                    CompactionData::builder()
                        .summary("## Goal\nContinue after replay".to_string())
                        .retained_tail(Vec::new())
                        .tokens_before(42_000)
                        .usage(None)
                        .details(Some(
                            CompactionDetails::builder()
                                .reason(CompactionReason::Manual)
                                .run_id(
                                    RunId::try_from("run-compaction")
                                        .expect("run id"),
                                )
                                .turn_id(
                                    TurnId::try_from("turn-compaction")
                                        .expect("turn id"),
                                )
                                .started_at_ms(TimestampMs::from(1_100))
                                .ended_at_ms(TimestampMs::from(1_200))
                                .read_files(Vec::new())
                                .modified_files(Vec::new())
                                .build(),
                        ))
                        .build(),
                )
                .expect("serialize compaction"),
            },
        )
        .expect("append compaction");
    let second_timestamp = TimestampMs::from(1_300);
    let second = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-after")
                .expect("message id"),
            turn_id: TurnId::try_from("turn-after").expect("turn id"),
        },
        timing: MessageTiming::try_from((
            second_timestamp,
            second_timestamp,
            second_timestamp,
        ))
        .expect("message timing"),
        content: MessageContent::User {
            blocks: vec![ContentBlock::Text {
                text: "after compaction".to_string(),
            }],
        },
    };
    store
        .append_entry(
            &protocol::LaneId::try_from("main").expect("lane id"),
            NewEntry {
                id: EntryId::try_from("entry-after").expect("entry id"),
                kind: EntryKind::Message,
                payload: serde_json::to_value(second)
                    .expect("serialize second message"),
            },
        )
        .expect("append second message");
    drop(store);

    let kernel = integration_kernel(state.path());
    let server = AcpServerFactory::new(kernel, Arc::new(NanoidIdGenerator))
        .component(AcpTransportKind::Stdio);
    let (updates_tx, mut updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = Client.v2().on_receive_notification(
        async move |notification: wire::UpdateSessionNotification,
                    _connection| {
            updates_tx.send(notification).map_err(|_error| {
                agent_client_protocol::Error::into_internal_error(
                    std::io::Error::other("notification receiver closed"),
                )
            })?;
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    );

    client
        .connect_with(server, async move |connection| {
            connection
                .send_request(wire::InitializeRequest::new(
                    ProtocolVersion::V2,
                    wire::Implementation::new(
                        "compaction-replay-test",
                        "0.1.0",
                    ),
                ))
                .block_task()
                .await?;
            connection
                .send_request(
                    wire::ResumeSessionRequest::new(
                        session_id.to_string(),
                        workspace.path().to_path_buf(),
                    )
                    .replay_from(wire::ReplayFrom::from(
                        wire::ReplayFromStart::new(),
                    )),
                )
                .block_task()
                .await?;
            let mut replay = Vec::new();
            loop {
                let notification = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    updates_rx.recv(),
                )
                .await
                .expect("replay update")
                .expect("replay notification");
                if matches!(
                    notification.update,
                    wire::SessionUpdate::AvailableCommandsUpdate(_)
                ) {
                    break;
                }
                replay.push(
                    serde_json::to_value(notification.update)
                        .expect("serialize update"),
                );
            }

            assert_eq!(replay.len(), 3);
            assert_eq!(replay[0]["sessionUpdate"], "user_message");
            assert_eq!(replay[0]["messageId"], "message-before");
            assert_eq!(
                replay[1]["sessionUpdate"],
                ProductIdentity::ACP_EVENT_UPDATE
            );
            assert_eq!(replay[1]["payload"]["event"], "compaction_end");
            assert_eq!(
                replay[1]["payload"]["outcome"]["result"]["entryId"],
                compaction_entry_id.as_str()
            );
            assert_eq!(replay[2]["sessionUpdate"], "user_message");
            assert_eq!(replay[2]["messageId"], "message-after");
            Ok(())
        })
        .await
        .expect("Compaction ACP replay lifecycle");
}
