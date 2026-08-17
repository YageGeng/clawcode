//! Kernel-owned MCP Host composition without client filesystem callbacks.

use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use futures::StreamExt;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, ContentBlock, EventMetadata,
    McpElicitationAction, McpElicitationRequest, McpElicitationResponseRequest,
    McpElicitationResult, McpElicitationSnapshot, McpHostRequest,
    McpHostResponse, McpRoot, McpRootsResult, McpSamplingRequest,
    McpSamplingResult, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelRequest, ModelRequestOptions, Role, Sequence,
    SessionId, TimestampMs, TurnId,
};
use tokio::sync::broadcast;

use super::{Kernel, KernelError, PendingMcpElicitation, SessionRuntime};

impl Kernel {
    /// Returns one Session's complete transient MCP elicitation state.
    pub fn mcp_elicitations(
        &self,
        session_id: &SessionId,
    ) -> Result<McpElicitationSnapshot, KernelError> {
        let session = self.session(session_id)?;
        let mut requests = session
            .pending_mcp_elicitations
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .values()
            .map(|pending| pending.request.clone())
            .collect::<Vec<_>>();
        requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));
        Ok(McpElicitationSnapshot {
            session_id: session_id.clone(),
            requests,
        })
    }
}

impl SessionRuntime {
    /// Starts the Session-owned projection from MCP snapshots into the MCP Tool partition.
    pub(super) async fn start_mcp_projection(
        self: &std::sync::Arc<Self>,
    ) -> Result<(), KernelError> {
        let Some(mcp) = self.mcp.as_ref().map(std::sync::Arc::clone) else {
            return Ok(());
        };
        let mut events = mcp.subscribe();
        let tool_state = std::sync::Arc::clone(&self.tool_state);
        let task = tokio::spawn(async move {
            loop {
                let change = match events.recv().await {
                    Ok(event) => event.change,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            "MCP Tool projection lagged by {} revisions; loading latest snapshot",
                            skipped
                        );
                        ::mcp::McpSessionChange::Catalog
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                match mcp.tool_registry().and_then(|registry| {
                    tool_state.replace_mcp_partition(registry).map_err(
                        |error| {
                            ::mcp::McpError::ToolRegistration(error.to_string())
                        },
                    )
                }) {
                    Ok(()) => {}
                    Err(error) => tracing::error!(
                        "MCP Tool projection failed for session '{}': {}",
                        mcp.session_id(),
                        error
                    ),
                }
                if change == ::mcp::McpSessionChange::Shutdown {
                    break;
                }
            }
        });
        *self.mcp_projection.lock().await = Some(task);
        Ok(())
    }

    /// Waits for the MCP projection task after the MCP Session publishes shutdown.
    pub(super) async fn stop_mcp_projection(&self) -> Result<(), KernelError> {
        if let Some(task) = self.mcp_projection.lock().await.take() {
            task.await.map_err(|error| {
                KernelError::Protocol(format!(
                    "MCP Tool projection task failed: {error}"
                ))
            })?;
        }
        Ok(())
    }

    /// Emits one MCP Host event through the active Turn sink and Session sequence.
    async fn emit_mcp_event(
        &self,
        turn_id: TurnId,
        timestamp_ms: TimestampMs,
        payload: AgentEventPayload,
    ) -> Result<(), ::mcp::McpError> {
        let sink = self
            .event_sink
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host(
                    "Session event sink lock poisoned".to_string(),
                )
            })?
            .clone()
            .ok_or_else(|| {
                ::mcp::McpError::Host(
                    "active Turn has no ACP event sink".to_string(),
                )
            })?;
        let raw_sequence = self
            .event_sequence
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| {
                ::mcp::McpError::Host(
                    "Session event sequence overflow".to_string(),
                )
            })?;
        let sequence = Sequence::try_from(raw_sequence)
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        sink.emit(AgentEvent {
            metadata: EventMetadata {
                turn_id,
                timestamp_ms,
                sequence,
            },
            payload,
        })
        .await
        .map_err(|error| ::mcp::McpError::Host(error.to_string()))
    }

    /// Publishes and waits for one Turn-scoped ACP elicitation response.
    async fn elicit_mcp(
        &self,
        request: McpElicitationRequest,
    ) -> Result<McpElicitationResult, ::mcp::McpError> {
        let request_id = request.request_id.clone();
        let turn_id = request.context.turn_id.clone();
        let requested_at_ms = request.context.requested_at_ms;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        match self
            .pending_mcp_elicitations
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host(
                    "MCP elicitation lock poisoned".to_string(),
                )
            })?
            .entry(request_id.clone())
        {
            Entry::Vacant(entry) => {
                entry.insert(PendingMcpElicitation {
                    request: request.clone(),
                    sender,
                });
            }
            Entry::Occupied(_entry) => {
                return Err(::mcp::McpError::Host(format!(
                    "duplicate MCP elicitation request '{request_id}'"
                )));
            }
        }
        if let Err(error) = self
            .emit_mcp_event(
                turn_id.clone(),
                requested_at_ms,
                AgentEventPayload::McpElicitationRequested {
                    request: request.clone(),
                },
            )
            .await
        {
            self.pending_mcp_elicitations
                .lock()
                .map_err(|_poison_error| {
                    ::mcp::McpError::Host(
                        "MCP elicitation lock poisoned".to_string(),
                    )
                })?
                .remove(&request_id);
            return Err(error);
        }
        let cancellation = self
            .cancellation
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host(
                    "Session cancellation lock poisoned".to_string(),
                )
            })?
            .clone();
        let result = tokio::select! {
            _ = cancellation.cancelled() => McpElicitationResult {
                action: McpElicitationAction::Cancel,
                content: None,
            },
            response = receiver => response.map_err(|_closed| {
                ::mcp::McpError::Host(format!(
                    "MCP elicitation '{request_id}' response channel closed"
                ))
            })?,
        };
        self.pending_mcp_elicitations
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host(
                    "MCP elicitation lock poisoned".to_string(),
                )
            })?
            .remove(&request_id);
        self.emit_mcp_event(
            turn_id,
            TimestampMs::now(),
            AgentEventPayload::McpElicitationResolved {
                request_id,
                result: result.clone(),
            },
        )
        .await?;
        Ok(result)
    }

    /// Delivers one ACP response to the exact pending MCP elicitation.
    pub(super) async fn resolve_mcp_elicitation(
        &self,
        request: McpElicitationResponseRequest,
    ) -> Result<(), KernelError> {
        let pending = self
            .pending_mcp_elicitations
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .remove(&request.request_id)
            .ok_or_else(|| {
                KernelError::Protocol(format!(
                    "MCP elicitation is not pending: {}",
                    request.request_id
                ))
            })?;
        pending.sender.send(request.result).map_err(|_result| {
            KernelError::Protocol(format!(
                "MCP elicitation is no longer accepting a response: {}",
                request.request_id
            ))
        })
    }
}

/// Session-local Host boundary exposing only roots the Kernel already owns.
pub(super) struct SessionMcpHost {
    cwd: PathBuf,
    runtime: OnceLock<Weak<SessionRuntime>>,
}

impl SessionMcpHost {
    /// Creates one Host restricted to the Agent Session working directory.
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            runtime: OnceLock::new(),
        }
    }

    /// Attaches the completed Session runtime without creating an ownership cycle.
    pub(super) fn attach(
        &self,
        runtime: &Arc<SessionRuntime>,
    ) -> Result<(), KernelError> {
        self.runtime
            .set(Arc::downgrade(runtime))
            .map_err(|_runtime| {
                KernelError::Protocol(
                    "MCP Host runtime was attached more than once".to_string(),
                )
            })
    }

    /// Resolves the live Session runtime for one Turn-scoped Host callback.
    fn runtime(&self) -> Result<Arc<SessionRuntime>, ::mcp::McpError> {
        self.runtime.get().and_then(Weak::upgrade).ok_or_else(|| {
            ::mcp::McpError::Host(
                "owning Kernel Session is no longer available".to_string(),
            )
        })
    }
}

impl SessionRuntime {
    /// Runs one Server-requested sample through the current Session model and cancellation path.
    async fn sample_mcp(
        &self,
        request: McpSamplingRequest,
    ) -> Result<McpSamplingResult, ::mcp::McpError> {
        let session_id = self
            .store
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host("Session Store lock poisoned".to_string())
            })?
            .session_id()
            .clone();
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
        let model = self
            .model
            .read()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host("Session model lock poisoned".to_string())
            })?
            .clone();
        model
            .preflight()
            .await
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        let cancellation = self
            .cancellation
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host(
                    "Session cancellation lock poisoned".to_string(),
                )
            })?
            .clone();
        let mut stream = model
            .stream(
                ModelRequest {
                    messages,
                    tools: Vec::new(),
                    options: ModelRequestOptions {
                        max_tokens: Some(request.max_tokens),
                        temperature: request.temperature,
                    },
                },
                cancellation,
            )
            .await
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        let mut blocks = Vec::new();
        let mut stop_reason = None;
        while let Some(event) = stream.next().await {
            match event
                .map_err(|error| ::mcp::McpError::Host(error.to_string()))?
            {
                protocol::ModelStreamEvent::TextDelta(text) => {
                    blocks.push(ContentBlock::Text { text });
                }
                protocol::ModelStreamEvent::ReasoningDelta(text) => {
                    blocks.push(ContentBlock::Reasoning { text });
                }
                protocol::ModelStreamEvent::ToolCall(call) => {
                    blocks.push(ContentBlock::ToolCall {
                        tool_call_id: call.tool_call_id,
                        name: call.name,
                        arguments: call.arguments,
                    });
                }
                protocol::ModelStreamEvent::Finished(final_) => {
                    stop_reason = Some(final_.stop_reason);
                }
            }
        }
        let stop_reason = stop_reason.ok_or_else(|| {
            ::mcp::McpError::Host(
                "Sampling Provider stream ended without a terminal event"
                    .to_string(),
            )
        })?;
        Ok(McpSamplingResult::builder()
            .role(Role::Assistant)
            .blocks(blocks)
            .stop_reason(stop_reason)
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
    /// Serves safe local roots and rejects Host capabilities not yet attached to a Turn.
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
                .runtime()?
                .elicit_mcp(request)
                .await
                .map(McpHostResponse::Elicitation),
            McpHostRequest::Sampling(request) => self
                .runtime()?
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
