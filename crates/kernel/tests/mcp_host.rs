use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::{Stream, stream};
use kernel::{EventSink, KernelFactory, Model, ModelError, ModelFactory};
use mcp::{
    McpClient, McpConnector, McpError, McpHost, McpMrtrPolicy,
    McpRequestControl, McpStdioTransport, McpTransport, RuntimeMcpServer,
    SessionMcpFactory,
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
    ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent, ModelUsage,
    RunRequest, SessionId, StopReason, TimestampMs, ToolCall, ToolCallId,
    TraceId,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

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
        ModelStreamEvent::Finished(ModelFinal {
            stop_reason: reason,
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

/// Kernel Sampling uses the current model and forwards request-local generation limits.
#[tokio::test]
async fn mcp_sampling_runs_through_current_session_model() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(1_000)));
    let model = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("mcp-call")
                        .expect("Tool call id"),
                    name: "mcp__sampling__sample".to_string(),
                    arguments: serde_json::json!({}),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ],
            vec![
                ModelStreamEvent::TextDelta("nested sample".to_string()),
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
        .protocol(McpProtocolVersion::V2025_11_25)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(1))
        .mrtr(McpMrtrPolicy {
            max_rounds: NonZeroU32::new(2).expect("non-zero rounds"),
            total_timeout: Duration::from_secs(2),
        })
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

    let result = kernel
        .run_traced(
            RunRequest {
                session_id,
                input: "use sampling".to_string(),
            },
            Arc::new(DiscardSink),
            TraceId::try_from("trace-sampling").expect("Trace id"),
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
    let requests = model.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].options.max_tokens, Some(64));
    assert_eq!(requests[1].options.temperature, Some(0.25));
    assert!(matches!(
        requests[1].messages.first().map(|message| &message.content),
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
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("mcp-elicit-call")
                        .expect("Tool call id"),
                    name: "mcp__sampling__sample".to_string(),
                    arguments: serde_json::json!({ "elicit": true }),
                }),
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
        .protocol(McpProtocolVersion::V2025_11_25)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(1))
        .mrtr(McpMrtrPolicy {
            max_rounds: NonZeroU32::new(2).expect("non-zero rounds"),
            total_timeout: Duration::from_secs(2),
        })
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
    let run = tokio::spawn(async move {
        run_kernel
            .run_traced(
                RunRequest {
                    session_id: run_session_id,
                    input: "request elicitation".to_string(),
                },
                Arc::new(DiscardSink),
                TraceId::try_from("trace-elicit").expect("Trace id"),
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
}
