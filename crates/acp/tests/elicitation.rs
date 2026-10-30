use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use agent_client_protocol::{Agent, Client, Responder, V2ConnectionTo};
use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::stream;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use mcp::{
    McpClient, McpConnector, McpError, McpHost, McpRequestControl,
    McpStdioTransport, McpTransport, RuntimeMcpServer, SessionMcpFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    ContentBlock, McpCatalog, McpCompletionRequest, McpCompletionResult,
    McpElicitationAction, McpElicitationMode, McpElicitationRequest,
    McpElicitationResult, McpHostRequest, McpHostResponse, McpPromptRequest,
    McpPromptResult, McpProtocolVersion, McpResourceRequest, McpResourceResult,
    McpServerCapabilities, McpServerId, McpServerImplementation, McpToolInfo,
    McpToolRef, McpToolRequest, McpToolResult, MessageContent, ModelFinal,
    ModelProfile, ModelRequest, ModelStreamEvent, ModelUsage, SessionId,
    StopReason,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Bounds every wait so a regression fails instead of hanging.
const WAIT: Duration = Duration::from_secs(5);

/// Gives an absent request time to arrive before the test asserts it never did.
const SETTLE: Duration = Duration::from_millis(300);

/// Form schema the fixture tool asks with unless a prompt selects another.
fn strategy_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "strategy": { "type": "string" } },
        "required": ["strategy"]
    })
}

/// Model that calls the eliciting MCP tool for "elicit…" prompts and answers otherwise.
struct ElicitingModel {
    profile: ModelProfile,
}

/// Finishes one scripted model attempt.
fn finished(stop_reason: StopReason) -> ModelStreamEvent {
    ModelStreamEvent::Finished(ModelFinal {
        stop_reason,
        raw_stop_reason: None,
        usage: ModelUsage::builder()
            .input_tokens(1)
            .output_tokens(1)
            .cache_read_tokens(0)
            .cache_write_tokens(0)
            .total_tokens(2)
            .build(),
    })
}

#[async_trait]
impl Model for ElicitingModel {
    /// Returns the deterministic fixture profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// The fixture is always ready.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Routes on the newest user text; tool results get a plain answer.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        let text = match request.messages.last().map(|message| &message.content)
        {
            Some(MessageContent::User { blocks }) => blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            _ => String::new(),
        };
        let arguments = match text.as_str() {
            "elicit" => serde_json::json!({ "schema": strategy_schema() }),
            // A property type JSON Schema does not define cannot compile.
            "elicit-bad-schema" => serde_json::json!({ "schema": {
                "type": "object",
                "properties": { "strategy": { "type": "wat" } }
            }}),
            _ => {
                return Ok(Box::pin(stream::iter([
                    Ok(ModelStreamEvent::TextDelta("answer".to_string())),
                    Ok(finished(StopReason::EndTurn)),
                ])));
            }
        };
        Ok(Box::pin(stream::iter([
            Ok(ModelStreamEvent::ToolCall(protocol::ToolCall {
                tool_call_id: protocol::ToolCallId::try_from("elicit-call")
                    .expect("tool call id"),
                name: "mcp__elicit__ask".to_string(),
                arguments,
            })),
            Ok(finished(StopReason::ToolUse)),
        ])))
    }
}

/// Shares one model across every Session.
struct ElicitingModelFactory(Arc<ElicitingModel>);

impl ModelFactory for ElicitingModelFactory {
    /// Returns the shared fixture model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }
}

/// Connects every Session to one fake MCP server that records elicitation results.
struct ElicitingConnector {
    results: mpsc::UnboundedSender<McpElicitationResult>,
    requests: Arc<AtomicUsize>,
}

#[async_trait]
impl McpConnector for ElicitingConnector {
    /// Retains the production Session Host in the fake remote client.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        _session_id: &SessionId,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Ok(Arc::new(ElicitingClient {
            server_id: server.server_id.clone(),
            host,
            capabilities: McpServerCapabilities::builder().tools(true).build(),
            results: self.results.clone(),
            requests: Arc::clone(&self.requests),
        }))
    }
}

/// Fake MCP server whose `ask` tool raises one form elicitation per call.
struct ElicitingClient {
    server_id: McpServerId,
    host: Arc<dyn McpHost>,
    capabilities: McpServerCapabilities,
    results: mpsc::UnboundedSender<McpElicitationResult>,
    requests: Arc<AtomicUsize>,
}

#[async_trait]
impl McpClient for ElicitingClient {
    /// Returns the structured route owner.
    fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Uses a revision that supports elicitation.
    fn protocol(&self) -> McpProtocolVersion {
        McpProtocolVersion::V2025_11_25
    }

    /// Omits optional implementation metadata.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        None
    }

    /// Declares only Tool support.
    fn capabilities(&self) -> &McpServerCapabilities {
        &self.capabilities
    }

    /// Exposes the single `ask` tool.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        let reference = McpToolRef {
            server_id: self.server_id.clone(),
            remote_name: "ask".to_string(),
        };
        Ok(McpCatalog::builder()
            .tools(vec![
                McpToolInfo::builder()
                    .reference(reference.clone())
                    .public_name(reference.public_name())
                    .input_schema(serde_json::json!({ "type": "object" }))
                    .build(),
            ])
            .build())
    }

    /// Asks the Host with the call's schema and reports what came back.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        let index = self.requests.fetch_add(1, Ordering::SeqCst);
        let McpHostResponse::Elicitation(result) = self
            .host
            .handle(McpHostRequest::Elicitation(McpElicitationRequest {
                request_id: format!("elicit-{index}"),
                context: control.context,
                mode: McpElicitationMode::Form {
                    message: "Pick a strategy".to_string(),
                    requested_schema: request
                        .arguments
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(strategy_schema),
                },
            }))
            .await?
        else {
            return Err(McpError::Host(
                "mismatched Elicitation result".to_string(),
            ));
        };
        let _ignored = self.results.send(result.clone());
        Ok(McpToolResult {
            content: vec![ContentBlock::Structured {
                value: serde_json::to_value(result)
                    .map_err(|error| McpError::Host(error.to_string()))?,
            }],
            structured_content: None,
            is_error: false,
        })
    }

    /// Prompt retrieval is not used by these tests.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        unreachable!("Prompt retrieval is not exercised")
    }

    /// Resource reading is not used by these tests.
    async fn read_resource(
        &self,
        _request: McpResourceRequest,
        _cancellation: &CancellationToken,
    ) -> Result<McpResourceResult, McpError> {
        unreachable!("Resource reading is not exercised")
    }

    /// Completion is not used by these tests.
    async fn complete(
        &self,
        _request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        unreachable!("Completion is not exercised")
    }

    /// No transport resources require shutdown.
    async fn shutdown(&self) -> Result<(), McpError> {
        Ok(())
    }
}

/// One Kernel with the eliciting MCP server, and what that server received.
struct Fixture {
    _state: tempfile::TempDir,
    workspace: tempfile::TempDir,
    kernel: Arc<Kernel>,
    acp: Arc<AcpServerFactory>,
    results: mpsc::UnboundedReceiver<McpElicitationResult>,
}

impl Fixture {
    /// Builds a production Kernel and ACP factory around the fake MCP server.
    fn new() -> Self {
        let state = tempfile::tempdir().expect("state");
        let workspace = tempfile::tempdir().expect("workspace");
        let (results_tx, results) = mpsc::unbounded_channel();
        let server = RuntimeMcpServer::builder()
            .server_id(McpServerId::try_from("elicit").expect("server id"))
            .enabled(true)
            .protocol_policy(protocol::McpProtocolPolicy::Auto)
            .startup_timeout(Duration::from_secs(5))
            .request_timeout(Duration::from_secs(30))
            .request_total_timeout(Duration::from_secs(60))
            .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
                command: "unused".to_string(),
                args: Vec::new(),
                env: Default::default(),
            })))
            .build();
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let model = Arc::new(ElicitingModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("eliciting".to_string())
                .display_name("Eliciting fixture".to_string())
                .context_tokens(1_000_000)
                .max_output_tokens(1_024)
                .build(),
        });
        let kernel = Arc::new(
            KernelFactory::builder()
                .model_factory(Arc::new(ElicitingModelFactory(model)))
                .tool_factory(Arc::new(BuiltinToolFactory::new()))
                .store_factory(Arc::new(JsonlStoreFactory::new(
                    state.path(),
                    Arc::clone(&clock),
                )))
                .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                    state.path().join("config"),
                    protocol::PromptPolicy::default(),
                )))
                .extension_factory(Arc::new(StaticExtensionFactory::default()))
                .mcp_factory(Some(Arc::new(SessionMcpFactory::new(
                    vec![server],
                    Arc::new(ElicitingConnector {
                        results: results_tx,
                        requests: Arc::new(AtomicUsize::new(0)),
                    }),
                ))))
                .clock(clock)
                .id_generator(Arc::new(NanoidIdGenerator))
                .build()
                .build()
                .expect("eliciting kernel"),
        );
        let acp = Arc::new(AcpServerFactory::new(
            Arc::clone(&kernel),
            Arc::new(NanoidIdGenerator),
            NonZeroUsize::new(128).expect("positive batch size"),
        ));
        Self {
            _state: state,
            workspace,
            kernel,
            acp,
            results,
        }
    }

    /// Waits for the next result the MCP server received.
    async fn next_result(&mut self) -> McpElicitationResult {
        tokio::time::timeout(WAIT, self.results.recv())
            .await
            .expect("MCP elicitation result")
            .expect("result stream")
    }

    /// Lists the elicitations the Kernel still holds for a Session.
    fn pending(&self, session_id: &wire::SessionId) -> usize {
        self.kernel
            .mcp_elicitations(
                &SessionId::try_from(session_id.to_string())
                    .expect("Session id"),
            )
            .expect("elicitation snapshot")
            .requests
            .len()
    }
}

/// One `elicitation/create` the client received, held until the test answers it.
type Asked = (
    wire::CreateElicitationRequest,
    Responder<wire::CreateElicitationResponse>,
);

/// Runs `body` on an initialized client that advertises `elicitation` when given.
async fn with_client<B>(
    factory: &Arc<AcpServerFactory>,
    elicitation: Option<wire::ElicitationCapabilities>,
    body: B,
) where
    B: AsyncFnOnce(
        &V2ConnectionTo<Agent>,
        &mut mpsc::UnboundedReceiver<Asked>,
    ) -> Result<(), agent_client_protocol::Error>,
{
    let server = factory.component(AcpTransportKind::Stdio);
    let (asked_tx, mut asked) = mpsc::unbounded_channel();
    Client
        .v2()
        .on_receive_request(
            async move |request: wire::CreateElicitationRequest,
                        responder: Responder<
                wire::CreateElicitationResponse,
            >,
                        _connection| {
                // Hand the request to the test body; answering here would stall dispatch.
                let _ignored = asked_tx.send((request, responder));
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(server, async move |connection| {
            connection
                .send_request(
                    wire::InitializeRequest::new(
                        ProtocolVersion::V2,
                        wire::Implementation::new("elicitation-test", "0.1.0"),
                    )
                    .capabilities(
                        wire::ClientCapabilities::new()
                            .elicitation(elicitation),
                    ),
                )
                .block_task()
                .await?;
            body(&connection, &mut asked).await
        })
        .await
        .expect("ACP connection");
}

/// Form-only elicitation support.
fn form() -> Option<wire::ElicitationCapabilities> {
    Some(
        wire::ElicitationCapabilities::new()
            .form(wire::ElicitationFormCapabilities::new()),
    )
}

/// Waits for the next `elicitation/create`.
async fn next_asked(asked: &mut mpsc::UnboundedReceiver<Asked>) -> Asked {
    tokio::time::timeout(WAIT, asked.recv())
        .await
        .expect("elicitation/create")
        .expect("request stream")
}

/// Asserts no `elicitation/create` arrives within `SETTLE`.
async fn assert_not_asked(asked: &mut mpsc::UnboundedReceiver<Asked>) {
    assert!(
        tokio::time::timeout(SETTLE, asked.recv()).await.is_err(),
        "unexpected elicitation/create"
    );
}

/// Creates a Session and submits one prompt on it.
async fn prompt_new_session(
    connection: &V2ConnectionTo<Agent>,
    workspace: &Path,
    text: &str,
) -> Result<wire::SessionId, agent_client_protocol::Error> {
    let created = connection
        .send_request(wire::NewSessionRequest::new(workspace.to_path_buf()))
        .block_task()
        .await?;
    prompt(connection, &created.session_id, text).await?;
    Ok(created.session_id)
}

/// Submits one text prompt; the response arrives once the prompt is inserted.
async fn prompt(
    connection: &V2ConnectionTo<Agent>,
    session_id: &wire::SessionId,
    text: &str,
) -> Result<(), agent_client_protocol::Error> {
    connection
        .send_request(wire::PromptRequest::new(
            session_id.clone(),
            vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
        ))
        .block_task()
        .await?;
    Ok(())
}

/// Resumes a Session from its start on this connection.
async fn resume(
    connection: &V2ConnectionTo<Agent>,
    session_id: &wire::SessionId,
    workspace: &Path,
) -> Result<(), agent_client_protocol::Error> {
    connection
        .send_request(
            wire::ResumeSessionRequest::new(
                session_id.clone(),
                workspace.to_path_buf(),
            )
            .replay_from(wire::ReplayFrom::from(wire::ReplayFromStart::new())),
        )
        .block_task()
        .await?;
    Ok(())
}

/// Accepts with one `strategy` value.
fn accept(
    strategy: wire::ElicitationContentValue,
) -> wire::CreateElicitationResponse {
    wire::CreateElicitationResponse::new(wire::ElicitationAction::Accept(
        wire::ElicitationAcceptAction::new()
            .content(BTreeMap::from([("strategy".to_string(), strategy)])),
    ))
}

/// A form-capable client gets the request scoped to its tool call, and its answer reaches MCP.
#[tokio::test]
async fn capable_client_receives_and_answers_elicitation() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    let (request, session_id) = {
        let mut seen = None;
        with_client(&fixture.acp, form(), async |connection, asked| {
            let session_id =
                prompt_new_session(connection, &workspace, "elicit").await?;
            let (request, responder) = next_asked(asked).await;
            responder.respond(accept(
                wire::ElicitationContentValue::String("balanced".to_string()),
            ))?;
            seen = Some((request, session_id));
            // Keep the connection until the answer reached MCP.
            tokio::time::sleep(SETTLE).await;
            Ok(())
        })
        .await;
        seen.expect("elicitation/create")
    };
    let json = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(json["mode"], "form");
    assert_eq!(json["message"], "Pick a strategy");
    assert_eq!(json["sessionId"], session_id.to_string());
    assert_eq!(json["toolCallId"], "elicit-call");
    assert_eq!(json["_meta"]["clawcode"]["mcpServerId"], "elicit");
    assert_eq!(json["requestedSchema"]["required"][0], "strategy");
    let result = fixture.next_result().await;
    assert_eq!(result.action, McpElicitationAction::Accept);
    assert_eq!(
        result.content,
        Some(serde_json::json!({ "strategy": "balanced" }))
    );
}

/// Once one connection answers, the other connection's copy is withdrawn.
#[tokio::test]
async fn second_connection_request_is_cancelled_after_first_answer() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    let acp = Arc::clone(&fixture.acp);
    with_client(&acp, form(), async |first, first_asked| {
        let created = first
            .send_request(wire::NewSessionRequest::new(workspace.clone()))
            .block_task()
            .await?;
        let session_id = created.session_id;
        with_client(&acp, form(), async |second, second_asked| {
            resume(second, &session_id, &workspace).await?;
            prompt(first, &session_id, "elicit").await?;
            let (_request, first_responder) = next_asked(first_asked).await;
            let (_request, second_responder) = next_asked(second_asked).await;
            let withdrawn = second_responder.cancellation();
            first_responder.respond(accept(
                wire::ElicitationContentValue::String("fast".to_string()),
            ))?;
            tokio::time::timeout(WAIT, withdrawn.cancelled())
                .await
                .expect("$/cancel_request for the unanswered copy");
            Ok(())
        })
        .await;
        Ok(())
    })
    .await;
    assert_eq!(
        fixture.next_result().await.action,
        McpElicitationAction::Accept
    );
}

/// A request left unanswered when its client leaves is asked again on resume.
#[tokio::test]
async fn pending_elicitation_is_reestablished_after_resume() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    let mut session_id = None;
    with_client(&fixture.acp, form(), async |connection, asked| {
        session_id =
            Some(prompt_new_session(connection, &workspace, "elicit").await?);
        // Received but never answered before this connection closes.
        let _unanswered = next_asked(asked).await;
        Ok(())
    })
    .await;
    let session_id = session_id.expect("Session id");
    assert_eq!(fixture.pending(&session_id), 1);
    with_client(&fixture.acp, form(), async |connection, asked| {
        resume(connection, &session_id, &workspace).await?;
        let (request, responder) = next_asked(asked).await;
        assert_eq!(request.message, "Pick a strategy");
        responder.respond(accept(wire::ElicitationContentValue::String(
            "thorough".to_string(),
        )))?;
        tokio::time::sleep(SETTLE).await;
        Ok(())
    })
    .await;
    let result = fixture.next_result().await;
    assert_eq!(
        result.content,
        Some(serde_json::json!({ "strategy": "thorough" }))
    );
}

/// Replaying history does not ask again for an elicitation that was already answered.
#[tokio::test]
async fn replay_does_not_resend_resolved_elicitation() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    let mut session_id = None;
    with_client(&fixture.acp, form(), async |connection, asked| {
        session_id =
            Some(prompt_new_session(connection, &workspace, "elicit").await?);
        let (_request, responder) = next_asked(asked).await;
        responder.respond(accept(wire::ElicitationContentValue::String(
            "balanced".to_string(),
        )))?;
        tokio::time::sleep(SETTLE).await;
        Ok(())
    })
    .await;
    fixture.next_result().await;
    let session_id = session_id.expect("Session id");
    with_client(&fixture.acp, form(), async |connection, asked| {
        resume(connection, &session_id, &workspace).await?;
        assert_not_asked(asked).await;
        Ok(())
    })
    .await;
}

/// An action this agent does not know is reported to MCP as a cancellation.
#[tokio::test]
async fn unknown_action_is_treated_as_cancel() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    with_client(&fixture.acp, form(), async |connection, asked| {
        prompt_new_session(connection, &workspace, "elicit").await?;
        let (_request, responder) = next_asked(asked).await;
        responder.respond(wire::CreateElicitationResponse::new(
            wire::ElicitationAction::Other(wire::OtherElicitationAction::new(
                "snooze",
                BTreeMap::new(),
            )),
        ))?;
        tokio::time::sleep(SETTLE).await;
        Ok(())
    })
    .await;
    assert_eq!(
        fixture.next_result().await.action,
        McpElicitationAction::Cancel
    );
}

/// Accepted content that violates the requested schema is reported as a cancellation.
#[tokio::test]
async fn schema_invalid_accept_is_sent_as_cancel() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    with_client(&fixture.acp, form(), async |connection, asked| {
        prompt_new_session(connection, &workspace, "elicit").await?;
        let (_request, responder) = next_asked(asked).await;
        responder.respond(accept(wire::ElicitationContentValue::Integer(3)))?;
        tokio::time::sleep(SETTLE).await;
        Ok(())
    })
    .await;
    let result = fixture.next_result().await;
    assert_eq!(result.action, McpElicitationAction::Cancel);
    assert_eq!(result.content, None);
}

/// A schema the bridge cannot compile is cancelled without asking the client.
#[tokio::test]
async fn uncompilable_requested_schema_is_cancelled_without_request() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    with_client(&fixture.acp, form(), async |connection, asked| {
        prompt_new_session(connection, &workspace, "elicit-bad-schema").await?;
        assert_not_asked(asked).await;
        Ok(())
    })
    .await;
    assert_eq!(
        fixture.next_result().await.action,
        McpElicitationAction::Cancel
    );
}

/// A client that does not advertise elicitation is never asked; the request stays pending.
#[tokio::test]
async fn client_without_capability_receives_no_request() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    with_client(&fixture.acp, None, async |connection, asked| {
        let session_id =
            prompt_new_session(connection, &workspace, "elicit").await?;
        assert_not_asked(asked).await;
        assert_eq!(fixture.pending(&session_id), 1);
        Ok(())
    })
    .await;
}

/// An answer arriving after the Turn was cancelled changes nothing.
#[tokio::test]
async fn late_response_after_turn_cancel_is_ignored() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace.path().to_path_buf();
    let results = &mut fixture.results;
    with_client(&fixture.acp, form(), async |connection, asked| {
        let session_id =
            prompt_new_session(connection, &workspace, "elicit").await?;
        let (_request, responder) = next_asked(asked).await;
        connection.send_notification(wire::CancelSessionNotification::new(
            session_id.clone(),
        ))?;
        let cancelled = tokio::time::timeout(WAIT, results.recv())
            .await
            .expect("MCP result after Turn cancel")
            .expect("result stream");
        assert_eq!(cancelled.action, McpElicitationAction::Cancel);
        responder.respond(accept(wire::ElicitationContentValue::String(
            "late".to_string(),
        )))?;
        tokio::time::sleep(SETTLE).await;
        // The connection keeps working after the ignored answer.
        prompt(connection, &session_id, "plain").await?;
        Ok(())
    })
    .await;
    fixture
        .results
        .try_recv()
        .expect_err("the late answer must not reach MCP");
}
