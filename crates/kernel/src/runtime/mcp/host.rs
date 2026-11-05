//! The MCP Host boundary: roots, sampling, and routing requests to live Sessions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};

use async_trait::async_trait;
use futures::StreamExt;
use protocol::{
    AgentMessage, ContentBlock, McpHostRequest, McpHostResponse, McpRoot,
    McpRootsResult, McpSamplingRequest, McpSamplingResult, MessageContent,
    MessageId, MessageIdentity, MessageTiming, ModelRequest,
    ModelRequestOptions, Role, SessionId, TimestampMs,
};

use super::{KernelError, Session};

/// Session-local Host boundary exposing only roots the Kernel already owns.
pub(in crate::runtime) struct SessionMcpHost {
    cwd: PathBuf,
    routes: RwLock<HashMap<SessionId, Weak<Session>>>,
}

impl SessionMcpHost {
    /// Creates one Host restricted to the Agent Session working directory.
    pub(in crate::runtime) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            routes: RwLock::new(HashMap::new()),
        }
    }

    /// Attaches the completed Session runtime without creating an ownership cycle.
    pub(in crate::runtime) fn attach(
        &self,
        runtime: &Arc<Session>,
    ) -> Result<(), KernelError> {
        let id = runtime.transcript.session_id()?;
        self.routes
            .write()?
            .insert(id.clone(), Arc::downgrade(runtime));
        tracing::info!(
            "attached MCP route for session {} to owner {}",
            id,
            runtime
                .ownership
                .as_ref()
                .map_or(&id, protocol::SessionOwnership::owner)
        );
        Ok(())
    }

    /// Removes a closed Session without disturbing routes for other shared leases.
    pub(in crate::runtime) fn detach(&self, session_id: &SessionId) {
        match self.routes.write() {
            Ok(mut routes) => {
                routes.remove(session_id);
                tracing::info!("detached MCP route for session {}", session_id);
            }
            Err(error) => tracing::warn!(
                "failed to detach MCP route for session {}: {}",
                session_id,
                error
            ),
        }
    }

    /// Resolves the live Session runtime for one Turn-scoped Host callback.
    fn runtime(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<Session>, ::mcp::McpError> {
        self.routes
            .read()
            .map_err(|_lock_error| {
                ::mcp::McpError::Host("MCP route lock poisoned".into())
            })?
            .get(session_id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                tracing::warn!(
                    "MCP Host route is unavailable for session {}",
                    session_id
                );
                ::mcp::McpError::Host(format!(
                    "MCP Host route is unavailable for session {session_id}"
                ))
            })
    }
}

impl Session {
    /// Runs one Server-requested sample through the current Session model and cancellation path.
    async fn sample_mcp(
        &self,
        request: McpSamplingRequest,
    ) -> Result<McpSamplingResult, ::mcp::McpError> {
        let session_id = self
            .transcript
            .session_id()
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        if session_id != request.context.session_id {
            return Err(::mcp::McpError::Host(format!(
                "Sampling Session mismatch: expected {}, received {}",
                session_id, request.context.session_id
            )));
        }
        let mut messages = request.messages;
        if let Some(system_prompt) = request.system_prompt {
            let timestamp = TimestampMs::now();
            messages.insert(
                0,
                AgentMessage {
                    identity: MessageIdentity {
                        message_id: MessageId::try_from(format!(
                            "mcp-sampling-system-{}",
                            request.context.trace_id
                        ))
                        .map_err(|error| {
                            ::mcp::McpError::Host(error.to_string())
                        })?,
                        turn_id: request.context.turn_id.clone(),
                    },
                    timing: MessageTiming::try_from((
                        timestamp, timestamp, timestamp,
                    ))
                    .map_err(|error| {
                        ::mcp::McpError::Host(error.to_string())
                    })?,
                    content: MessageContent::System {
                        blocks: vec![ContentBlock::Text {
                            text: system_prompt,
                        }],
                    },
                },
            );
        }
        let model = self.model.active().map_err(|error| {
            ::mcp::McpError::Host(format!(
                "failed to read Session model state: {error}"
            ))
        })?;
        model
            .preflight()
            .await
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        let cancellation = self
            .execution
            .cancellation()
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        let mut model_request = ModelRequest {
            messages,
            tools: Vec::new(),
            // MCP sampling never reasons; the session level applies to Turns only.
            options: ModelRequestOptions::builder()
                .max_tokens(Some(request.max_tokens))
                .temperature(request.temperature)
                .thinking_level(protocol::ThinkingLevel::Off)
                .build(),
        };
        if model_request.adapt_input(model.profile()) {
            tracing::debug!(
                "replaced unsupported images before MCP sampling model {}/{}",
                model.profile().provider_id,
                model.profile().model_id
            );
        }
        let mut stream = model
            .stream(model_request, cancellation)
            .await
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        let mut blocks = Vec::new();
        let mut final_result = None;
        while let Some(event) = stream.next().await {
            match event
                .map_err(|error| ::mcp::McpError::Host(error.to_string()))?
            {
                protocol::ModelStreamEvent::TextDelta(delta) => {
                    // Older Sampling revisions accept one text item, not streaming fragments.
                    match blocks.last_mut() {
                        Some(ContentBlock::Text { text }) => {
                            text.push_str(&delta)
                        }
                        _ => blocks.push(ContentBlock::Text { text: delta }),
                    }
                }
                // Reasoning has no MCP Sampling wire representation and must not
                // make an otherwise valid visible answer fail serialization.
                protocol::ModelStreamEvent::ReasoningDelta(_) => {}
                protocol::ModelStreamEvent::ToolCall(call) => {
                    blocks.push(ContentBlock::ToolCall {
                        invalid_arguments: call.invalid_arguments,
                        tool_call_id: call.tool_call_id,
                        name: call.name,
                        arguments: call.arguments,
                    });
                }
                protocol::ModelStreamEvent::Finished(final_) => {
                    final_result = Some(final_);
                }
            }
        }
        let final_result = final_result.ok_or_else(|| {
            ::mcp::McpError::Host(
                "Sampling Provider stream ended without a terminal event"
                    .to_string(),
            )
        })?;
        if let Some(native) = final_result.model_message.as_deref() {
            // Sampling must observe the same final correction as ordinary turns.
            blocks = crate::provider::AssistantProjection::from(native)
                .blocks()
                .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
            blocks.retain(|block| {
                !matches!(block, ContentBlock::Reasoning { .. })
            });
        }
        Ok(McpSamplingResult::builder()
            .role(Role::Assistant)
            .blocks(blocks)
            .stop_reason(final_result.stop_reason)
            .model(format!(
                "{}/{}",
                model.profile().provider_id,
                model.profile().model_id
            ))
            .build())
    }
}

#[async_trait]
impl ::mcp::McpHost for SessionMcpHost {
    /// Serves Session roots independently of Turn state and dispatches Turn-scoped callbacks.
    async fn handle(
        &self,
        request: McpHostRequest,
    ) -> Result<McpHostResponse, ::mcp::McpError> {
        match request {
            McpHostRequest::Roots(_request) => {
                let uri = url::Url::from_directory_path(&self.cwd)
                    .map_err(|()| {
                        ::mcp::McpError::Host(format!(
                            "session cwd is not an absolute filesystem path: {:?}",
                            self.cwd
                        ))
                    })?
                    .to_string();
                Ok(McpHostResponse::Roots(McpRootsResult {
                    roots: vec![McpRoot {
                        uri,
                        name: self
                            .cwd
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned()),
                    }],
                }))
            }
            McpHostRequest::Elicitation(request) => self
                .runtime(&request.context.session_id)?
                .elicit_mcp(request)
                .await
                .map(McpHostResponse::Elicitation),
            McpHostRequest::Sampling(request) => self
                .runtime(&request.context.session_id)?
                .sample_mcp(request)
                .await
                .map(McpHostResponse::Sampling),
            McpHostRequest::Authorization(_request) => {
                Err(::mcp::McpError::Host(
                    "OAuth authorization is not initialized".to_string(),
                ))
            }
        }
    }
}
