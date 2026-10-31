//! Kernel-owned MCP Host composition without client filesystem callbacks.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use futures::StreamExt;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, ContentBlock, EventMetadata,
    McpElicitationAction, McpElicitationRequest, McpElicitationResponseRequest,
    McpElicitationResult, McpElicitationSnapshot, McpHostRequest,
    McpHostResponse, McpRoot, McpRootsResult, McpSamplingRequest,
    McpSamplingResult, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelRequest, ModelRequestOptions, Role, SessionId,
    TimestampMs, TurnId,
};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use super::{Kernel, KernelError, PendingMcpElicitation, Session};

// Recreated Session runtimes must outrank snapshots retained by an attached WebUI.
static MCP_UI_REVISION: AtomicU64 = AtomicU64::new(1);

/// Represents an absent or fully initialized Session-scoped MCP capability.
pub(super) enum SessionMcpRuntime {
    Disabled,
    Enabled(SessionMcpState),
}

/// Determines whether construction opens transports or borrows its owner's connection.
pub(super) enum SessionMcpSource<'a> {
    Connect,
    Share(&'a SessionMcpRuntime),
}

/// Only the transport owner may reconnect or shut down the shared MCP session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum McpLease {
    Owned,
    Shared,
}

/// Groups the MCP session, projection task, and pending elicitations.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct SessionMcpState {
    pub(super) session: Arc<::mcp::McpSession>,
    pub(super) host: Arc<SessionMcpHost>,
    session_id: SessionId,
    lease: McpLease,
    #[builder(default)]
    projection: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    elicitations: Mutex<HashMap<String, PendingMcpElicitation>>,
    #[builder(default = Mutex::new(McpProjectionNotifications::default()))]
    notifications: Mutex<McpProjectionNotifications>,
}

/// Serializes UI invalidations without storing a second catalog or changing resource cursors.
pub(super) struct McpProjectionNotifications {
    revision: u64,
    sender: broadcast::Sender<::mcp::McpSessionEvent>,
}

impl Default for McpProjectionNotifications {
    /// Starts a connection-independent UI revision stream for this Session runtime.
    fn default() -> Self {
        Self {
            revision: MCP_UI_REVISION.fetch_add(1, Ordering::Relaxed),
            sender: broadcast::channel(128).0,
        }
    }
}

impl SessionMcpRuntime {
    /// Converts an optional MCP Session into the explicit capability state.
    pub(super) fn new(
        session: Option<(Arc<::mcp::McpSession>, Arc<SessionMcpHost>)>,
        session_id: SessionId,
        lease: McpLease,
    ) -> Self {
        session.map_or(Self::Disabled, |(session, host)| {
            Self::Enabled(
                SessionMcpState::builder()
                    .session(session)
                    .host(host)
                    .session_id(session_id)
                    .lease(lease)
                    .projection(AsyncMutex::new(None))
                    .elicitations(Mutex::new(HashMap::new()))
                    .build(),
            )
        })
    }

    /// Returns the enabled MCP Session handle when configured.
    pub(super) fn as_ref(&self) -> Option<&Arc<::mcp::McpSession>> {
        match self {
            Self::Disabled => None,
            Self::Enabled(state) => Some(&state.session),
        }
    }

    /// Returns enabled MCP state for projection and elicitation operations.
    pub(super) fn state(&self) -> Option<&SessionMcpState> {
        match self {
            Self::Disabled => None,
            Self::Enabled(state) => Some(state),
        }
    }

    /// Rejects transport management from a shared child's view of the connection.
    pub(super) fn ensure_owned(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        if self
            .state()
            .is_some_and(|state| state.lease == McpLease::Shared)
        {
            tracing::warn!(
                "refused MCP management from shared session {}",
                session_id
            );
            return Err(KernelError::SharedMcpManagement(session_id.clone()));
        }
        Ok(())
    }

    /// Shuts down the MCP Session and always joins its projection task.
    pub(super) async fn shutdown(&self) -> Result<(), KernelError> {
        let Some(state) = self.state() else {
            return Ok(());
        };
        tracing::info!("started MCP shutdown for session {}", state.session_id);
        state.host.detach(&state.session_id);
        let shutdown_result = match state.lease {
            McpLease::Owned => state.session.shutdown().await,
            McpLease::Shared => Ok(()),
        };
        let projection_result =
            if let Some(task) = state.projection.lock().await.take() {
                if state.lease == McpLease::Shared {
                    task.abort();
                }
                match task.await {
                    Ok(()) => Ok(()),
                    Err(error)
                        if state.lease == McpLease::Shared
                            && error.is_cancelled() =>
                    {
                        Ok(())
                    }
                    Err(error) => Err(KernelError::Protocol(format!(
                        "MCP Tool projection task failed: {error}"
                    ))),
                }
            } else {
                Ok(())
            };
        let result: Result<(), KernelError> = match (
            shutdown_result,
            projection_result,
        ) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error.into()),
            (Err(error), Err(projection_error)) => {
                // Keep the MCP shutdown failure primary while preserving the
                // independently attempted projection cleanup in diagnostics.
                tracing::warn!(
                    "MCP projection cleanup also failed after shutdown error: {}",
                    projection_error
                );
                Err(error.into())
            }
        };
        match &result {
            Ok(()) => tracing::info!(
                "completed MCP shutdown for session {}",
                state.session_id
            ),
            Err(error) => tracing::error!(
                "failed MCP shutdown for session {}: {}",
                state.session_id,
                error
            ),
        }
        result
    }
}

impl Kernel {
    /// Returns one Session's complete transient MCP elicitation state.
    pub fn mcp_elicitations(
        &self,
        session_id: &SessionId,
    ) -> Result<McpElicitationSnapshot, KernelError> {
        let session = self.session(session_id)?;
        let Some(state) = session.mcp.state() else {
            return Ok(McpElicitationSnapshot {
                session_id: session_id.clone(),
                requests: Vec::new(),
            });
        };
        let mut requests = state
            .elicitations
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

impl Session {
    /// Derives the public catalog view from current server policy and host preferences.
    pub(super) fn mcp_snapshot(
        &self,
    ) -> Result<protocol::McpSessionSnapshot, KernelError> {
        let Some(state) = self.mcp.state() else {
            return Ok(protocol::McpSessionSnapshot::default());
        };
        let notifications = state
            .notifications
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let selection = self.tools.snapshot()?;
        let mut snapshot = state.session.snapshot().as_ref().clone();
        snapshot.revision = notifications.revision;
        for tool in &mut snapshot.catalog.tools {
            tool.disabled =
                selection.selection.disabled.contains(&tool.public_name);
            tool.loaded =
                selection.declares(&tool.public_name, tool.metadata.exposure);
        }
        Ok(snapshot)
    }

    /// Subscribes to both server changes and durable selection changes through one ACP channel.
    pub(super) fn subscribe_mcp_projection(
        &self,
    ) -> Result<Option<broadcast::Receiver<::mcp::McpSessionEvent>>, KernelError>
    {
        self.mcp
            .state()
            .map(|state| {
                state
                    .notifications
                    .lock()
                    .map(|notifications| notifications.sender.subscribe())
                    .map_err(|_poison_error| KernelError::Poisoned)
            })
            .transpose()
    }

    /// Publishes one monotonic invalidation after the corresponding state has become visible.
    pub(super) fn notify_mcp_change(
        &self,
        change: protocol::McpSessionChange,
        server_id: Option<protocol::McpServerId>,
    ) -> Result<(), KernelError> {
        let Some(state) = self.mcp.state() else {
            return Ok(());
        };
        let mut notifications = state
            .notifications
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        notifications.revision =
            MCP_UI_REVISION.fetch_add(1, Ordering::Relaxed);
        let _ = notifications.sender.send(::mcp::McpSessionEvent {
            revision: notifications.revision,
            server_id,
            change,
        });
        Ok(())
    }

    /// Starts the Session-owned projection from MCP snapshots into the MCP Tool partition.
    pub(super) async fn start_mcp_projection(
        self: &std::sync::Arc<Self>,
    ) -> Result<(), KernelError> {
        let Some(state) = self.mcp.state() else {
            return Ok(());
        };
        let mcp = Arc::clone(&state.session);
        let mut events = mcp.subscribe();
        let mut published = mcp.snapshot();
        // Subscribe before reconciling startup: events between initial discovery and attachment must not be lost.
        self.tools.replace_mcp_partition(
            mcp.tool_registry_from_snapshot(&published)?,
        )?;
        let tools = std::sync::Arc::clone(&self.tools);
        let owner = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            loop {
                let (change, server_id) = match events.recv().await {
                    Ok(event) => (event.change, event.server_id),
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            "MCP Tool projection lagged by {} revisions; loading latest snapshot",
                            skipped
                        );
                        (::mcp::McpSessionChange::Catalog, None)
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let current = mcp.snapshot();
                // Status-only notifications do not invalidate search documents or compiled validators.
                if current.catalog.tools != published.catalog.tools
                    || change == ::mcp::McpSessionChange::Shutdown
                {
                    match mcp.tool_registry_from_snapshot(&current).and_then(
                        |registry| {
                            tools.replace_mcp_partition(registry).map_err(
                                |error| {
                                    ::mcp::McpError::ToolRegistration(
                                        error.to_string(),
                                    )
                                },
                            )
                        },
                    ) {
                        Ok(()) => {}
                        Err(error) => tracing::error!(
                            "MCP Tool projection failed for session '{}': {}",
                            mcp.session_id(),
                            error
                        ),
                    }
                }
                published = current;
                let Some(owner) = owner.upgrade() else {
                    break;
                };
                if let Err(error) = owner.notify_mcp_change(change, server_id) {
                    tracing::error!(
                        "MCP projection notification failed: {}",
                        error
                    );
                }
                if change == ::mcp::McpSessionChange::Shutdown {
                    break;
                }
            }
        });
        *state.projection.lock().await = Some(task);
        Ok(())
    }

    /// Emits one MCP Host event through the active Turn sink and Session sequence.
    async fn emit_mcp_event(
        &self,
        turn_id: TurnId,
        timestamp_ms: TimestampMs,
        payload: AgentEventPayload,
    ) -> Result<(), ::mcp::McpError> {
        let active_run = self
            .execution
            .active_run()
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?
            .ok_or_else(|| {
                ::mcp::McpError::Host(
                    "active Turn has no ACP event sink".to_string(),
                )
            })?;
        let sequence = self
            .execution
            .next_sequence()
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        active_run
            .sink
            .emit(AgentEvent {
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
        let state = self.mcp.state().ok_or_else(|| {
            ::mcp::McpError::Host(
                "MCP is disabled for this Session".to_string(),
            )
        })?;
        // The first pending request blocks the Run on the user.
        let first_pending = {
            let mut elicitations =
                state.elicitations.lock().map_err(|_poison_error| {
                    ::mcp::McpError::Host(
                        "MCP elicitation lock poisoned".to_string(),
                    )
                })?;
            match elicitations.entry(request_id.clone()) {
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
            elicitations.len() == 1
        };
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
            state
                .elicitations
                .lock()
                .map_err(|_poison_error| {
                    ::mcp::McpError::Host(
                        "MCP elicitation lock poisoned".to_string(),
                    )
                })?
                .remove(&request_id);
            return Err(error);
        }
        if first_pending {
            tracing::info!(
                "Run requires user action for MCP elicitation {}",
                request_id
            );
            self.emit_mcp_event(
                turn_id.clone(),
                requested_at_ms,
                AgentEventPayload::ActionRequired {
                    request_id: request_id.clone(),
                },
            )
            .await?;
        }
        let cancellation = self
            .execution
            .cancellation()
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
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
        let last_resolved = {
            let mut elicitations =
                state.elicitations.lock().map_err(|_poison_error| {
                    ::mcp::McpError::Host(
                        "MCP elicitation lock poisoned".to_string(),
                    )
                })?;
            elicitations.remove(&request_id);
            elicitations.is_empty()
        };
        self.emit_mcp_event(
            turn_id.clone(),
            TimestampMs::now(),
            AgentEventPayload::McpElicitationResolved {
                request_id: request_id.clone(),
                result: result.clone(),
            },
        )
        .await?;
        // The Run is still waiting on this tool, so it resumes foreground work.
        if last_resolved {
            tracing::info!(
                "Run resumed after MCP elicitation {} was resolved",
                request_id
            );
            self.emit_mcp_event(
                turn_id,
                TimestampMs::now(),
                AgentEventPayload::ActionResolved { request_id },
            )
            .await?;
        }
        Ok(result)
    }

    /// Delivers one ACP response to the exact pending MCP elicitation.
    pub(super) async fn resolve_mcp_elicitation(
        &self,
        request: McpElicitationResponseRequest,
    ) -> Result<(), KernelError> {
        let state = self.mcp.state().ok_or_else(|| {
            KernelError::Protocol(
                "MCP is disabled for this Session".to_string(),
            )
        })?;
        let pending = state
            .elicitations
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .remove(&request.request_id)
            .ok_or_else(|| {
                // Another client answered first, or the Turn cancelled it.
                tracing::info!(
                    "ignored response for MCP elicitation {} that is no longer pending",
                    request.request_id
                );
                KernelError::ElicitationResolved(request.request_id.clone())
            })?;
        pending.sender.send(request.result).map_err(|_result| {
            tracing::info!(
                "MCP elicitation {} stopped waiting before its response arrived",
                request.request_id
            );
            KernelError::ElicitationResolved(request.request_id.clone())
        })
    }
}

/// Session-local Host boundary exposing only roots the Kernel already owns.
pub(super) struct SessionMcpHost {
    cwd: PathBuf,
    routes: RwLock<HashMap<SessionId, Weak<Session>>>,
}

impl SessionMcpHost {
    /// Creates one Host restricted to the Agent Session working directory.
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            routes: RwLock::new(HashMap::new()),
        }
    }

    /// Attaches the completed Session runtime without creating an ownership cycle.
    pub(super) fn attach(
        &self,
        runtime: &Arc<Session>,
    ) -> Result<(), KernelError> {
        let id = runtime.transcript.session_id()?;
        self.routes
            .write()
            .map_err(|_lock_error| KernelError::Poisoned)?
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
    pub(super) fn detach(&self, session_id: &SessionId) {
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
            options: ModelRequestOptions {
                max_tokens: Some(request.max_tokens),
                temperature: request.temperature,
            },
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

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};

    use async_trait::async_trait;
    use protocol::{McpHostRequest, McpHostResponse, SessionId};
    use tokio::sync::Mutex as AsyncMutex;
    use tokio_util::sync::CancellationToken;

    use super::{McpLease, SessionMcpHost, SessionMcpRuntime, SessionMcpState};

    /// Process-wide MCP test capture buffer written by the global subscriber.
    static BUFFER: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

    /// Returns the shared capture buffer, installing the global subscriber once.
    ///
    /// `set_default` is thread-local and is lost when a `tokio` task migrates
    /// between worker threads, which makes lifecycle log capture flaky. A global
    /// subscriber retains every emitted log on whichever thread it fires.
    fn buffer() -> &'static Arc<Mutex<Vec<u8>>> {
        BUFFER.get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(CapturedLogWriter(Arc::clone(&buf)))
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("global MCP tracing subscriber already installed");
            buf
        })
    }

    /// Shared in-memory writer used to inspect MCP lifecycle logs.
    #[derive(Clone)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        /// Returns all UTF-8 tracing output written by the subscriber.
        fn content(&self) -> String {
            String::from_utf8(self.0.lock().expect("log lock").clone())
                .expect("UTF-8 logs")
        }
    }

    impl Default for CapturedLogs {
        /// Creates a handle over the shared capture buffer.
        fn default() -> Self {
            CapturedLogs(Arc::clone(buffer()))
        }
    }

    /// One writer handle backed by the shared test buffer.
    struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogWriter {
        /// Appends one formatted tracing buffer to the shared capture.
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
                .write(buffer)
        }

        /// The in-memory capture has no buffered state to flush.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
        type Writer = CapturedLogWriter;

        /// Creates a writer handle sharing the captured byte buffer.
        fn make_writer(&'writer self) -> Self::Writer {
            CapturedLogWriter(Arc::clone(&self.0))
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer>
        for CapturedLogWriter
    {
        type Writer = CapturedLogWriter;

        /// Creates a writer handle sharing the captured byte buffer.
        fn make_writer(&'writer self) -> Self::Writer {
            CapturedLogWriter(Arc::clone(&self.0))
        }
    }

    struct RejectingHost;

    #[async_trait]
    impl ::mcp::McpHost for RejectingHost {
        /// Rejects callbacks because the empty MCP Session has no Servers.
        async fn handle(
            &self,
            _request: McpHostRequest,
        ) -> Result<McpHostResponse, ::mcp::McpError> {
            Err(::mcp::McpError::Host(
                "unexpected test Host callback".to_string(),
            ))
        }
    }

    /// Creates one empty MCP Session with an observable shutdown token.
    async fn mcp_session(shutdown: CancellationToken) -> ::mcp::McpSession {
        use ::mcp::McpFactory as _;

        ::mcp::SessionMcpFactory::new(
            Vec::new(),
            Arc::new(::mcp::RmcpConnector::default()),
        )
        .create(
            ::mcp::McpSessionRequest::builder()
                .session_id(
                    SessionId::try_from("session-mcp-cleanup")
                        .expect("session id"),
                )
                .cwd(PathBuf::from("/workspace"))
                .host(Arc::new(RejectingHost) as Arc<dyn ::mcp::McpHost>)
                .shutdown(shutdown)
                .build(),
        )
        .await
        .expect("create empty MCP Session")
    }

    /// Preserves MCP shutdown even when the projection task exits abnormally.
    #[tokio::test]
    async fn shutdown_cancels_mcp_when_projection_join_fails() {
        let shutdown = CancellationToken::new();
        let session = Arc::new(mcp_session(shutdown.clone()).await);
        let failed_projection = tokio::spawn(async {
            panic!("forced projection failure");
        });
        let runtime = SessionMcpRuntime::Enabled(
            SessionMcpState::builder()
                .session(session)
                .host(Arc::new(SessionMcpHost::new(PathBuf::from(
                    "/workspace",
                ))))
                .session_id(
                    SessionId::try_from("session-mcp-cleanup").expect("id"),
                )
                .lease(McpLease::Owned)
                .projection(AsyncMutex::new(Some(failed_projection)))
                .elicitations(Mutex::new(std::collections::HashMap::new()))
                .build(),
        );

        let error = runtime
            .shutdown()
            .await
            .expect_err("projection failure must remain observable");

        assert!(shutdown.is_cancelled());
        assert!(matches!(error, crate::KernelError::Protocol(_)));
    }

    /// MCP shutdown logs both third-party call boundaries with Session context.
    #[tokio::test]
    async fn shutdown_lifecycle_is_logged_with_session_context() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let shutdown = CancellationToken::new();
        let session = Arc::new(mcp_session(shutdown.clone()).await);
        let runtime = SessionMcpRuntime::Enabled(
            SessionMcpState::builder()
                .session(session)
                .host(Arc::new(SessionMcpHost::new(PathBuf::from(
                    "/workspace",
                ))))
                .session_id(
                    SessionId::try_from("session-mcp-cleanup").expect("id"),
                )
                .lease(McpLease::Owned)
                .projection(AsyncMutex::new(None))
                .elicitations(Mutex::new(std::collections::HashMap::new()))
                .build(),
        );

        tracing_futures::WithSubscriber::with_subscriber(
            runtime.shutdown(),
            dispatch,
        )
        .await
        .expect("shut down MCP runtime");

        assert!(shutdown.is_cancelled());
        let output = logs.content();
        for lifecycle in [
            "started MCP shutdown for session session-mcp-cleanup",
            "completed MCP shutdown for session session-mcp-cleanup",
        ] {
            assert!(
                output.contains(lifecycle),
                "missing {lifecycle} in captured logs: {output:?}"
            );
        }
    }
}
