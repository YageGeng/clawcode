use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::{Stream, stream};
use kernel::{EventSink, KernelFactory, Model, ModelError, ModelFactory};
use mcp::{
    McpClient, McpConnector, McpError, McpHost, McpRequestControl,
    McpStdioTransport, McpTransport, RuntimeMcpServer, SessionMcpFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentMessage, ContentBlock, IdGenerator, IdKind, McpCatalog,
    McpCompletionRequest, McpCompletionResult, McpElicitationAction,
    McpElicitationMode, McpElicitationRequest, McpElicitationResponseRequest,
    McpElicitationResult, McpHostRequest, McpHostResponse, McpPromptRequest,
    McpPromptResult, McpProtocolVersion, McpResourceRequest, McpResourceResult,
    McpSamplingRequest, McpServerCapabilities, McpServerId,
    McpServerImplementation, McpToolInfo, McpToolRef, McpToolRequest,
    McpToolResult, MessageContent, MessageId, MessageIdentity, MessageTiming,
    ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent, RunRequest,
    SessionId, StopReason, TimestampMs, ToolCall, ToolCallId, TraceId,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Disabled MCP keeps public snapshot defaults and request errors stable.
#[tokio::test]
async fn disabled_mcp_preserves_public_semantics() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(0)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build Kernel");
    let session_id =
        SessionId::try_from("session-disabled-mcp").expect("Session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create Session");

    assert_eq!(
        kernel.mcp_status(&session_id).expect("MCP status"),
        protocol::McpSessionSnapshot::default()
    );
    assert!(
        kernel
            .subscribe_mcp(&session_id)
            .expect("subscribe")
            .is_none()
    );
    assert!(
        kernel
            .mcp_elicitations(&session_id)
            .expect("elicitations")
            .requests
            .is_empty()
    );
    let error = kernel
        .mcp_reconnect(protocol::McpReconnectRequest {
            session_id,
            server_id: McpServerId::try_from("missing").expect("Server id"),
        })
        .await
        .expect_err("disabled reconnect");
    assert!(matches!(
        error,
        kernel::KernelError::Mcp(McpError::ServerNotFound(_))
    ));
}

/// Stable profile used by the nested Sampling model fixture.
static PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("nested".to_string())
        .display_name("Nested Sampling".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

/// Advances deterministic persisted timestamps.
struct StepClock(Mutex<u64>);

impl Clock for StepClock {
    /// Returns the next deterministic millisecond value.
    fn now(&self) -> TimestampMs {
        let mut value = self.0.lock().expect("clock lock");
        *value += 1;
        TimestampMs::from(*value)
    }
}

/// Generates readable identifiers for the Kernel lifecycle.
struct SequentialIds(Mutex<u64>);

impl IdGenerator for SequentialIds {
    /// Returns one monotonically increasing domain identifier.
    fn next(&self, kind: IdKind) -> String {
        let mut value = self.0.lock().expect("id lock");
        *value += 1;
        format!("{}-{}", kind.prefix(), *value)
    }
}

/// Streams main-Turn and nested-Sampling scripts from one shared model.
struct ScriptedModel {
    scripts: Mutex<VecDeque<Vec<ModelStreamEvent>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    /// Builds one deterministic terminal provider event.
    fn finished(reason: StopReason) -> ModelStreamEvent {
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(reason)
                .raw_stop_reason(None)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new()
                        .input_tokens(1)
                        .output_tokens(1)
                        .cached_input_tokens(0)
                        .cache_creation_input_tokens(0)
                        .total_tokens(2),
                ))
                .build(),
        )
    }
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the stable nested-Sampling model profile.
    fn profile(&self) -> &ModelProfile {
        &PROFILE
    }

    /// Confirms this fixture has no external setup.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Captures the request and returns the next scripted stream.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.requests.lock().expect("request lock").push(request);
        let events = self
            .scripts
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("scripted response");
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// Returns the shared scripted model for each Session runtime.
struct StaticModelFactory(Arc<dyn Model>);

impl ModelFactory for StaticModelFactory {
    /// Clones the shared model trait object.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Creates a Tool-only client that invokes Sampling through the supplied Host.
struct SamplingConnector;

/// Counts real factory connections while reusing the existing callback-capable client.
#[derive(Default)]
struct CountingConnector {
    connections: std::sync::atomic::AtomicUsize,
    host: Mutex<Option<Arc<dyn McpHost>>>,
}

#[async_trait]
impl McpConnector for CountingConnector {
    /// Records connection ownership before delegating to the remote boundary fixture.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        session_id: &SessionId,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        self.connections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.host.lock().expect("host capture") = Some(Arc::clone(&host));
        SamplingConnector.connect(server, session_id, host).await
    }
}

/// Owns persistent storage and callback-capable MCP state for lifecycle tests.
#[derive(typed_builder::TypedBuilder)]
struct OwnedMcpFixture {
    root: tempfile::TempDir,
    kernel: kernel::Kernel,
    connector: Arc<CountingConnector>,
    owner: SessionId,
}

impl OwnedMcpFixture {
    /// Builds a real Kernel and one MCP connection whose tool can elicit or sample.
    async fn new(scripts: Vec<Vec<ModelStreamEvent>>) -> Self {
        let root = tempfile::tempdir().expect("root");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(0)));
        let model = Arc::new(ScriptedModel {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
        });
        let connector = Arc::new(CountingConnector::default());
        let server = RuntimeMcpServer::builder()
            .server_id(McpServerId::try_from("sampling").expect("id"))
            .enabled(true)
            .exposure(protocol::ToolExposure::Direct)
            .protocol_policy(protocol::McpProtocolPolicy::Auto)
            .startup_timeout(Duration::from_secs(2))
            .request_timeout(Duration::from_secs(5))
            .request_total_timeout(Duration::from_secs(10))
            .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
                command: "unused".into(),
                args: Vec::new(),
                env: Default::default(),
            })))
            .build();
        let kernel = KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.path().join("config"),
                Default::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .mcp_factory(Some(Arc::new(SessionMcpFactory::new(
                vec![server],
                Arc::clone(&connector) as Arc<dyn McpConnector>,
            ))))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
            .build()
            .build()
            .expect("kernel");
        let owner = SessionId::try_from("owner").expect("id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: owner.clone(),
                cwd: root.path().into(),
                parent_session_id: None,
            })
            .await
            .expect("owner");
        Self::builder()
            .root(root)
            .kernel(kernel)
            .connector(connector)
            .owner(owner)
            .build()
    }

    /// Creates a foreground child through the public ownership primitive.
    async fn child(&self) -> SessionId {
        self.kernel
            .create_owned_session(
                protocol::SessionOwnership::Tool {
                    session_id: self.owner.clone(),
                    tool_call_id: ToolCallId::try_from("call-1").expect("call"),
                },
                protocol::SessionTitle::try_from("child").expect("title"),
            )
            .await
            .expect("child")
    }

    /// Samples through the real Session host captured by the connection boundary.
    async fn sample(&self) -> protocol::McpSamplingResult {
        let host = Arc::clone(
            self.connector
                .host
                .lock()
                .expect("host capture")
                .as_ref()
                .expect("connected host"),
        );
        let timestamp = TimestampMs::from(1000);
        let turn_id =
            protocol::TurnId::try_from("sampling-turn").expect("turn ID");
        let context = protocol::McpRequestContext::builder()
            .server_id(McpServerId::try_from("sampling").expect("server ID"))
            .session_id(self.owner.clone())
            .turn_id(turn_id.clone())
            .trace_id(TraceId::try_from("sampling-trace").expect("trace ID"))
            .requested_at_ms(timestamp)
            .build();
        let message = AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("sampling-user")
                    .expect("message ID"),
                turn_id,
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("timing"),
            content: MessageContent::User {
                blocks: vec![ContentBlock::Text {
                    text: "sample".into(),
                }],
            },
        };
        let response = host
            .handle(McpHostRequest::Sampling(
                McpSamplingRequest::builder()
                    .context(context)
                    .messages(vec![message])
                    .max_tokens(64)
                    .build(),
            ))
            .await
            .expect("sample response");
        let McpHostResponse::Sampling(result) = response else {
            unreachable!("host must answer Sampling with a sampling result")
        };
        result
    }

    /// Reads the actual number of transport connections made by the factory.
    fn connects(&self) -> usize {
        self.connector
            .connections
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Children reuse the owner's transport and cannot reconnect it independently.
#[tokio::test]
async fn owned_session_reuses_owner_mcp_connection() {
    let test = OwnedMcpFixture::new(Vec::new()).await;
    let child = test.child().await;
    assert_eq!(test.connects(), 1);
    assert!(
        !test
            .kernel
            .mcp_status(&child)
            .expect("catalog")
            .catalog
            .tools
            .is_empty()
    );
    assert!(matches!(
        test.kernel
            .mcp_reconnect(protocol::McpReconnectRequest {
                session_id: child,
                server_id: McpServerId::try_from("sampling").expect("id")
            })
            .await,
        Err(kernel::KernelError::SharedMcpManagement(_))
    ));
    test.kernel.close_session(&test.owner).await.expect("close");
}

/// A child callback must enter the child's pending table and its active event sink.
#[tokio::test]
async fn child_elicitation_routes_to_child_session() {
    let test = OwnedMcpFixture::new(vec![
        vec![
            ModelStreamEvent::ToolCall(
                ToolCall::builder()
                    .tool_call_id(ToolCallId::try_from("elicit").expect("call"))
                    .name("mcp__sampling__sample".into())
                    .arguments(serde_json::json!({"elicit":true}))
                    .build(),
            ),
            ScriptedModel::finished(StopReason::ToolUse),
        ],
        vec![ScriptedModel::finished(StopReason::EndTurn)],
    ])
    .await;
    let child = test.child().await;
    let kernel = test.kernel.clone();
    let id = child.clone();
    let run = tokio::spawn(async move {
        kernel
            .run(
                RunRequest {
                    session_id: id,
                    input: "elicit".into(),
                },
                Arc::new(DiscardSink),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while test
            .kernel
            .mcp_elicitations(&child)
            .expect("child")
            .requests
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child elicitation");
    assert!(
        test.kernel
            .mcp_elicitations(&test.owner)
            .expect("owner")
            .requests
            .is_empty()
    );
    test.kernel
        .mcp_respond_elicitation(McpElicitationResponseRequest {
            session_id: child,
            request_id: "pending-elicit".into(),
            result: McpElicitationResult {
                action: McpElicitationAction::Accept,
                content: Some(serde_json::json!({"value":"accepted"})),
            },
        })
        .await
        .expect("respond");
    run.await.expect("join").expect("run");
    test.kernel.close_session(&test.owner).await.expect("close");
}

/// Closing a shared child cannot close its owner's transport.
#[tokio::test]
async fn closing_child_keeps_owner_mcp_alive() {
    let test = OwnedMcpFixture::new(vec![
        vec![
            ModelStreamEvent::ToolCall(
                ToolCall::builder()
                    .tool_call_id(ToolCallId::try_from("sample").expect("call"))
                    .name("mcp__sampling__sample".into())
                    .arguments(serde_json::json!({}))
                    .build(),
            ),
            ScriptedModel::finished(StopReason::ToolUse),
        ],
        vec![
            ModelStreamEvent::TextDelta("sampled".into()),
            ScriptedModel::finished(StopReason::EndTurn),
        ],
        vec![ScriptedModel::finished(StopReason::EndTurn)],
    ])
    .await;
    let child = test.child().await;
    test.kernel
        .close_session(&child)
        .await
        .expect("child close");
    let result = test
        .kernel
        .run(
            RunRequest {
                session_id: test.owner.clone(),
                input: "sample".into(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("owner run");
    assert!(result.messages.iter().any(|m| matches!(
        &m.content,
        MessageContent::ToolResult {
            is_error: false,
            ..
        }
    )
        && m.content.text_content().as_deref() == Some("sampled")));
    test.kernel.close_session(&test.owner).await.expect("close");
}

/// Resuming a closed child first recreates its owner and then shares the new connection.
#[tokio::test]
async fn resuming_child_resumes_owner_first() {
    let test = OwnedMcpFixture::new(Vec::new()).await;
    let child = test.child().await;
    test.kernel.close_session(&test.owner).await.expect("close");
    assert!(matches!(
        test.kernel.session_transcript(&child),
        Err(kernel::KernelError::SessionNotFound(_))
    ));
    test.kernel
        .resume_session(child.clone(), test.root.path().into())
        .await
        .expect("resume");
    test.kernel
        .session_transcript(&test.owner)
        .expect("owner live");
    assert_eq!(test.connects(), 2);
    test.kernel.close_session(&test.owner).await.expect("close");
}

/// Missing owner files permit orphan recovery with a separately owned MCP lease.
#[tokio::test]
async fn resuming_orphan_child_connects_its_own_mcp() {
    let test = OwnedMcpFixture::new(Vec::new()).await;
    let child = test.child().await;
    test.kernel.close_session(&test.owner).await.expect("close");
    let factory = JsonlStoreFactory::new(
        test.root.path(),
        Arc::new(StepClock(Mutex::new(0))),
    );
    store::StoreFactory::delete(&factory, &test.owner)
        .expect("remove only owner file");
    let before = test.connects();
    test.kernel
        .resume_session(child.clone(), test.root.path().into())
        .await
        .expect("orphan resume");
    assert_eq!(test.connects(), before + 1);
    test.kernel
        .close_session(&child)
        .await
        .expect("close orphan");
}

/// Deleting an owner removes both live and previously closed children's logs.
#[tokio::test]
async fn deleting_owner_deletes_child_store() {
    let test = OwnedMcpFixture::new(Vec::new()).await;
    let child = test.child().await;
    test.kernel
        .close_session(&child)
        .await
        .expect("close child");
    test.kernel
        .delete_session(&test.owner)
        .await
        .expect("delete");
    assert!(
        test.kernel
            .list_sessions(Some(test.root.path()))
            .expect("list")
            .is_empty()
    );
}

#[async_trait]
impl McpConnector for SamplingConnector {
    /// Retains the production Session Host in one fake remote client.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        _session_id: &SessionId,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Ok(Arc::new(SamplingClient {
            server_id: server.server_id.clone(),
            host,
            capabilities: McpServerCapabilities::builder().tools(true).build(),
        }))
    }
}

/// Simulates a Server whose Tool requests one nested model sample.
struct SamplingClient {
    server_id: McpServerId,
    host: Arc<dyn McpHost>,
    capabilities: McpServerCapabilities,
}

#[async_trait]
impl McpClient for SamplingClient {
    /// Returns the structured route owner.
    fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Uses the configured Legacy revision.
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

    /// Exposes one stable remote Tool definition.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        let reference = McpToolRef {
            server_id: self.server_id.clone(),
            remote_name: "sample".to_string(),
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

    /// Invokes Host Sampling with the exact Tool request correlation.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        if request
            .arguments
            .get("elicit")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            let result = self
                .host
                .handle(McpHostRequest::Elicitation(McpElicitationRequest {
                    request_id: "pending-elicit".to_string(),
                    context: control.context,
                    mode: McpElicitationMode::Form {
                        message: "Choose a value".to_string(),
                        requested_schema: serde_json::json!({
                            "type": "object",
                            "properties": { "value": { "type": "string" } }
                        }),
                    },
                }))
                .await?;
            let McpHostResponse::Elicitation(result) = result else {
                return Err(McpError::Host(
                    "mismatched Elicitation result".to_string(),
                ));
            };
            return Ok(McpToolResult {
                content: vec![ContentBlock::Structured {
                    value: serde_json::to_value(result)
                        .map_err(|error| McpError::Host(error.to_string()))?,
                }],
                structured_content: None,
                is_error: false,
            });
        }
        let timestamp = TimestampMs::now();
        let request = McpSamplingRequest::builder()
            .context(control.context.clone())
            .messages(vec![AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("nested-user")
                        .expect("message id"),
                    turn_id: control.context.turn_id.clone(),
                },
                timing: MessageTiming::try_from((
                    timestamp, timestamp, timestamp,
                ))
                .expect("message timing"),
                content: MessageContent::User {
                    blocks: vec![ContentBlock::Text {
                        text: "sample this".to_string(),
                    }],
                },
            }])
            .system_prompt(Some("nested system".to_string()))
            .max_tokens(64)
            .temperature(Some(0.25))
            .build();
        let McpHostResponse::Sampling(result) =
            self.host.handle(McpHostRequest::Sampling(request)).await?
        else {
            return Err(McpError::Host(
                "mismatched Sampling result".to_string(),
            ));
        };
        Ok(McpToolResult {
            content: result.blocks,
            structured_content: None,
            is_error: false,
        })
    }

    /// Prompt retrieval is not used by this test.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        unreachable!("Prompt retrieval is not exercised")
    }

    /// Resource reading is not used by this test.
    async fn read_resource(
        &self,
        _request: McpResourceRequest,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<McpResourceResult, McpError> {
        unreachable!("Resource reading is not exercised")
    }

    /// Completion is not used by this test.
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

/// Accepts every emitted event without changing runtime behavior.
struct DiscardSink;

#[async_trait]
impl EventSink for DiscardSink {
    /// Discards one emitted event.
    async fn emit(&self, _event: AgentEvent) -> Result<(), kernel::SinkError> {
        Ok(())
    }
}

/// Sampling keeps the visible answer while excluding reasoning from the MCP wire content.
#[tokio::test]
async fn mcp_sampling_runs_through_current_session_model() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(1_000)));
    let model = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            vec![
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("discover-sampling")
                                .expect("id"),
                        )
                        .name("tool_search".into())
                        .arguments(serde_json::json!({"query":"sample"}))
                        .build(),
                ),
                ScriptedModel::finished(StopReason::ToolUse),
            ],
            vec![
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("mcp-call")
                                .expect("Tool call id"),
                        )
                        .name("mcp__sampling__sample".to_string())
                        .arguments(serde_json::json!({}))
                        .build(),
                ),
                ScriptedModel::finished(StopReason::ToolUse),
            ],
            vec![
                ModelStreamEvent::ReasoningDelta(
                    "private reasoning".to_string(),
                ),
                ModelStreamEvent::TextDelta("nested ".to_string()),
                ModelStreamEvent::TextDelta("sample".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ],
            vec![
                ModelStreamEvent::TextDelta("done".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ],
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let server = RuntimeMcpServer::builder()
        .server_id(McpServerId::try_from("sampling").expect("Server id"))
        .enabled(true)
        .exposure(protocol::ToolExposure::Deferred)
        .protocol_policy(protocol::McpProtocolPolicy::Auto)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(1))
        .request_total_timeout(Duration::from_secs(2))
        .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
            command: "unused".to_string(),
            args: Vec::new(),
            env: Default::default(),
        })))
        .build();
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(&model) as Arc<dyn Model>
        )))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .mcp_factory(Some(Arc::new(SessionMcpFactory::new(
            vec![server],
            Arc::new(SamplingConnector),
        ))))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build Kernel");
    let session_id =
        SessionId::try_from("session-sampling").expect("Session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: PathBuf::from(temporary.path()),
            parent_session_id: None,
        })
        .await
        .expect("create Session");

    let before = kernel.mcp_status(&session_id).expect("before");
    assert!(before.catalog.tools.iter().all(|tool| !tool.loaded));
    let mut changes = kernel
        .subscribe_mcp(&session_id)
        .expect("subscribe")
        .expect("MCP enabled");
    let result = kernel
        .run_traced(
            RunRequest {
                session_id: session_id.clone(),
                input: "use sampling".into(),
            },
            Arc::new(DiscardSink),
            TraceId::try_from("trace-sampling").expect("Trace id"),
            None,
        )
        .await
        .expect("run Kernel");

    assert!(result.messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::ToolResult { blocks, .. }
                if blocks.iter().any(|block| block.text() == Some("nested sample"))
        )
    }));
    assert!(result.messages.iter().all(|message| {
        !matches!(
            &message.content,
            MessageContent::ToolResult { blocks, .. }
                if blocks.iter().any(|block| matches!(block, ContentBlock::Reasoning { .. }))
        )
    }), "Sampling must not return reasoning blocks rejected by the MCP encoder");
    let after = kernel.mcp_status(&session_id).expect("after");
    assert!(after.catalog.tools.iter().all(|tool| tool.loaded));
    assert!(after.revision > before.revision);
    let change = changes.try_recv().expect("selection notification");
    assert_eq!(change.change, protocol::McpSessionChange::Selection);
    let requests = model.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].options.max_tokens, Some(64));
    assert_eq!(requests[2].options.temperature, Some(0.25));
    assert!(matches!(
        requests[2].messages.first().map(|message| &message.content),
        Some(MessageContent::System { .. })
    ));
}

/// Pending MCP elicitation requests remain queryable until their exact response resolves them.
#[tokio::test]
async fn pending_mcp_elicitations_have_session_snapshots() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(2_000)));
    let model = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            vec![
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("mcp-elicit-call")
                                .expect("Tool call id"),
                        )
                        .name("mcp__sampling__sample".to_string())
                        .arguments(serde_json::json!({ "elicit": true }))
                        .build(),
                ),
                ScriptedModel::finished(StopReason::ToolUse),
            ],
            vec![
                ModelStreamEvent::TextDelta("elicitation resolved".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ],
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let server = RuntimeMcpServer::builder()
        .server_id(McpServerId::try_from("sampling").expect("Server id"))
        .enabled(true)
        .protocol_policy(protocol::McpProtocolPolicy::Auto)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(1))
        .request_total_timeout(Duration::from_secs(2))
        .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
            command: "unused".to_string(),
            args: Vec::new(),
            env: Default::default(),
        })))
        .build();
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(
                Arc::clone(&model) as Arc<dyn Model>
            )))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                temporary.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                temporary.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .mcp_factory(Some(Arc::new(SessionMcpFactory::new(
                vec![server],
                Arc::new(SamplingConnector),
            ))))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
            .build()
            .build()
            .expect("build Kernel"),
    );
    let session_id = SessionId::try_from("session-elicit").expect("Session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create Session");
    let run_kernel = Arc::clone(&kernel);
    let run_session_id = session_id.clone();
    let (events, mut received) = tokio::sync::mpsc::channel(256);
    let run = tokio::spawn(async move {
        run_kernel
            .run_traced(
                RunRequest {
                    session_id: run_session_id,
                    input: "request elicitation".into(),
                },
                Arc::new(kernel::ChannelEventSink::new(events)),
                TraceId::try_from("trace-elicit").expect("Trace id"),
                None,
            )
            .await
    });

    let snapshot = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let snapshot = kernel
                .mcp_elicitations(&session_id)
                .expect("MCP elicitation snapshot");
            if !snapshot.requests.is_empty() {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending elicitation must become queryable");
    assert_eq!(snapshot.session_id, session_id);
    assert_eq!(snapshot.requests[0].request_id, "pending-elicit");
    // The request names the MCP tool call that raised it.
    assert_eq!(
        snapshot.requests[0]
            .context
            .tool_call_id
            .as_ref()
            .map(ToolCallId::as_str),
        Some("mcp-elicit-call")
    );

    kernel
        .mcp_respond_elicitation(McpElicitationResponseRequest {
            session_id: session_id.clone(),
            request_id: "pending-elicit".to_string(),
            result: McpElicitationResult {
                action: McpElicitationAction::Accept,
                content: Some(serde_json::json!({ "value": "accepted" })),
            },
        })
        .await
        .expect("resolve elicitation");
    // A second answer, such as another client's, learns it was already resolved.
    let second = kernel
        .mcp_respond_elicitation(McpElicitationResponseRequest {
            session_id: session_id.clone(),
            request_id: "pending-elicit".to_string(),
            result: McpElicitationResult {
                action: McpElicitationAction::Decline,
                content: None,
            },
        })
        .await;
    assert!(matches!(
        second,
        Err(kernel::KernelError::ElicitationResolved(_))
    ));
    run.await
        .expect("run task")
        .expect("run after elicitation response");
    assert!(
        kernel
            .mcp_elicitations(&session_id)
            .expect("resolved snapshot")
            .requests
            .is_empty()
    );
    // A pending elicitation blocks the Run, so it is reported as required action.
    let mut lifecycle = Vec::new();
    while let Ok(event) = received.try_recv() {
        match event.payload {
            protocol::AgentEventPayload::McpElicitationRequested { .. } => {
                lifecycle.push("requested");
            }
            protocol::AgentEventPayload::ActionRequired { request_id } => {
                assert_eq!(request_id, "pending-elicit");
                lifecycle.push("action_required");
            }
            protocol::AgentEventPayload::McpElicitationResolved { .. } => {
                lifecycle.push("resolved");
            }
            protocol::AgentEventPayload::ActionResolved { request_id } => {
                assert_eq!(request_id, "pending-elicit");
                lifecycle.push("action_resolved");
            }
            _ => {}
        }
    }
    assert_eq!(
        lifecycle,
        vec![
            "requested",
            "action_required",
            "resolved",
            "action_resolved"
        ]
    );
}

/// Native final content replaces stale deltas, including final-only output, without leaking reasoning.
#[tokio::test]
async fn sampling_uses_final_native_content() {
    for deltas in [false, true] {
        let mut events = Vec::new();
        if deltas {
            events.push(ModelStreamEvent::TextDelta("stale".into()));
        }
        let native = rig_core::message::AssistantMessage::new(vec![
            rig_core::message::AssistantContent::Reasoning(
                rig_core::message::Reasoning::new("private"),
            ),
            rig_core::message::AssistantContent::text("corrected"),
        ])
        .with_stop(rig_core::message::StopReason::Stop);
        events.push(ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(StopReason::EndTurn)
                .usage(protocol::ModelUsage::default())
                .model_message(Some(Box::new(native)))
                .build(),
        ));
        let fixture = OwnedMcpFixture::new(vec![events]).await;
        let result = fixture.sample().await;
        assert_eq!(
            result.blocks,
            vec![ContentBlock::Text {
                text: "corrected".into()
            }]
        );
    }
}

/// Both incremental-only and native final paths retain malformed arguments for MCP wire rejection.
#[tokio::test]
async fn sampling_preserves_invalid_tool_arguments() {
    for native in [false, true] {
        let call = ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("bad").expect("call ID"))
            .name("remote_tool".into())
            .arguments(serde_json::json!({}))
            .invalid_arguments(Some("{".into()))
            .build();
        let final_message = native.then(|| {
            Box::new(
                rig_core::message::AssistantMessage::new(vec![
                    rig_core::message::AssistantContent::ToolCall(
                        rig_core::message::ToolCall::from_wire(
                            "bad",
                            rig_core::message::ToolFunction::new(
                                rig_core::message::ToolName::new("remote_tool")
                                    .expect("name"),
                                serde_json::json!("{"),
                            ),
                        ),
                    ),
                ])
                .with_stop(rig_core::message::StopReason::ToolUse),
            )
        });
        let fixture = OwnedMcpFixture::new(vec![vec![
            ModelStreamEvent::ToolCall(call),
            ModelStreamEvent::Finished(
                ModelFinal::builder()
                    .stop_reason(StopReason::ToolUse)
                    .usage(protocol::ModelUsage::default())
                    .model_message(final_message)
                    .build(),
            ),
        ]])
        .await;
        let result = fixture.sample().await;
        assert!(
            matches!(result.blocks.as_slice(),[ContentBlock::ToolCall {invalid_arguments:Some(raw),arguments,..}] if raw=="{" && arguments==&serde_json::json!({}))
        );
    }
}
