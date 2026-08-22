use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use agent_client_protocol::{
    Agent, Client, ConnectTo, ConnectionTo, JsonRpcMessage, Responder,
};
use kernel::{Kernel, KernelError};
use protocol::{
    AcpExtensionMethod, AcpWorkingDirectory, AgentEvent, AgentEventPayload,
    EventMetadata, IdGenerator, McpSessionChange,
    McpSessionRevisionNotification, ProductIdentity, RunRequest, Sequence,
    SessionId, TimestampMs,
};
use tokio_util::sync::CancellationToken;

use crate::AcpEventMapper;
use crate::batching::BatchComponent;
use crate::extension::{
    AcpExtensionDispatcher, AcpExtensionRequest, AcpMcpUpdateNotification,
};
use crate::input::PromptInput;
use crate::projection::{
    DurableResumeSelection, EventGroup, ProjectionFeed, ProjectionRegistry,
};
use crate::recovery::{
    AcpReplayRequest, CursorErrorData, RECOVERY_VERSION, RecoveryMode,
    RecoveryRequest, RecoveryResponse, ResumeMeta,
};
use crate::trace::AcpTraceFactory;

/// Cohesive ACP notification operations backed by one shared Kernel.
pub(crate) struct AcpServer {
    kernel: Arc<Kernel>,
    projections: Arc<ProjectionRegistry>,
}

/// Snapshot and live stream installed for one connection-level Session watcher.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct ProjectionWatch {
    backlog: Vec<Arc<EventGroup>>,
    receiver: tokio::sync::broadcast::Receiver<Arc<EventGroup>>,
    #[builder(default, setter(strip_option))]
    run_id: Option<protocol::RunId>,
    #[builder(default, setter(strip_option))]
    next_sequence: Option<Sequence>,
}

impl From<ProjectionFeed> for ProjectionWatch {
    /// Converts an atomic recovery snapshot into a connection watcher input.
    fn from(subscription: ProjectionFeed) -> Self {
        Self::builder()
            .backlog(subscription.backlog)
            .receiver(subscription.receiver)
            .run_id(subscription.run_id)
            .next_sequence(subscription.next_sequence)
            .build()
    }
}

impl ProjectionWatch {
    /// Advances the recoverable cursor after one projection group is sent.
    fn record_sent(&mut self, group: &EventGroup) {
        let Some(run_id) = &group.run_id else {
            // Ungrouped updates do not invalidate the last recoverable journal
            // cursor; explicit projection invalidation is handled by the registry.
            return;
        };
        self.run_id = Some(run_id.clone());
        self.next_sequence = group
            .sequence
            .get()
            .checked_add(1)
            .and_then(|sequence| Sequence::try_from(sequence).ok());
    }
}

impl AcpServer {
    /// Binds ACP notification operations to one Kernel instance.
    pub(crate) fn new(
        kernel: Arc<Kernel>,
        projections: Arc<ProjectionRegistry>,
    ) -> Self {
        Self {
            kernel,
            projections,
        }
    }

    /// Sends the complete Session command snapshot through native ACP v2.
    pub(crate) fn send_available_commands(
        &self,
        session_id: &SessionId,
        connection: &ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let event = self
            .kernel
            .available_commands_event(session_id)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        let metadata = AcpEventMapper::metadata(&event)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        for update in AcpEventMapper::map(event)
            .map_err(agent_client_protocol::Error::into_internal_error)?
        {
            connection.send_notification(
                wire::UpdateSessionNotification::new(
                    session_id.to_string(),
                    update,
                )
                .meta(metadata.clone()),
            )?;
        }
        Ok(())
    }

    /// Sends durable replay items through their standard ACP Session updates.
    fn send_replay_items(
        &self,
        session_id: &SessionId,
        connection: &ConnectionTo<Client>,
        items: Vec<protocol::SessionReplayItem>,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut event_index = 0_usize;
        for item in items {
            let events = match item {
                protocol::SessionReplayItem::Message {
                    message,
                    context_window,
                } => {
                    let usage = match (&message.content, context_window) {
                        (
                            protocol::MessageContent::Assistant {
                                metadata,
                                ..
                            },
                            Some(context_window),
                        ) => Some((metadata.usage.clone(), context_window)),
                        _ => None,
                    };
                    let turn_id = message.identity.turn_id.clone();
                    let timestamp_ms = message.timing.ended_at_ms;
                    // Replay preserves the live MessageEnd -> UsageUpdated order
                    // so clients rebuild Context without a separate snapshot.
                    let mut events = vec![(
                        turn_id.clone(),
                        timestamp_ms,
                        AgentEventPayload::MessageEnd { message },
                    )];
                    if let Some((usage, context_window)) = usage {
                        events.push((
                            turn_id,
                            timestamp_ms,
                            AgentEventPayload::UsageUpdated {
                                usage,
                                context_window,
                            },
                        ));
                    }
                    events
                }
                protocol::SessionReplayItem::Turn(turn) => vec![(
                    turn.identity.turn_id.clone(),
                    turn.timing.ended_at_ms,
                    AgentEventPayload::TurnEnd { turn },
                )],
                protocol::SessionReplayItem::RunEnd {
                    run_id,
                    turn_id,
                    outcome,
                    ended_at_ms,
                } => vec![(
                    turn_id,
                    ended_at_ms,
                    AgentEventPayload::RunEnd { run_id, outcome },
                )],
                protocol::SessionReplayItem::Compaction {
                    run_id,
                    reason,
                    result,
                } => vec![(
                    result.turn_id.clone(),
                    result.ended_at_ms,
                    AgentEventPayload::CompactionEnd {
                        run_id,
                        reason,
                        outcome: protocol::CompactionOutcome::Completed {
                            result,
                        },
                    },
                )],
            };
            for (turn_id, timestamp_ms, payload) in events {
                event_index = event_index.saturating_add(1);
                let event = AgentEvent {
                    metadata: EventMetadata {
                        turn_id,
                        timestamp_ms,
                        sequence: Sequence::try_from(
                            u64::try_from(event_index).unwrap_or(u64::MAX),
                        )
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?,
                    },
                    payload,
                };
                let metadata = AcpEventMapper::metadata(&event).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
                for update in AcpEventMapper::map(event).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )? {
                    connection.send_notification(
                        wire::UpdateSessionNotification::new(
                            session_id.to_string(),
                            update,
                        )
                        .meta(metadata.clone()),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Starts one deduplicated MCP revision watcher for this ACP connection and Session.
    pub(crate) fn watch_mcp(
        &self,
        session_id: &SessionId,
        connection: &ConnectionTo<Client>,
        watched: Arc<Mutex<BTreeSet<SessionId>>>,
    ) -> Result<(), agent_client_protocol::Error> {
        let Some(mut events) = self
            .kernel
            .subscribe_mcp(session_id)
            .map_err(agent_client_protocol::Error::into_internal_error)?
        else {
            return Ok(());
        };
        {
            let mut sessions = watched.lock().map_err(|_poison_error| {
                agent_client_protocol::Error::into_internal_error(
                    std::io::Error::other("ACP MCP watcher lock poisoned"),
                )
            })?;
            if !sessions.insert(session_id.clone()) {
                return Ok(());
            }
        }
        let kernel = Arc::clone(&self.kernel);
        let session_id = session_id.clone();
        let task_connection = connection.clone();
        connection.spawn(async move {
            loop {
                let (revision, server_id, change) = match events.recv().await {
                    Ok(event) => {
                        (event.revision, event.server_id, event.change)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(
                        _,
                    )) => {
                        let snapshot = kernel.mcp_status(&session_id).map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?;
                        (snapshot.revision, None, McpSessionChange::Catalog)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                };
                let notification = McpSessionRevisionNotification::builder()
                    .session_id(session_id.clone())
                    .revision(revision)
                    .server_id(server_id)
                    .change(change)
                    .timestamp_ms(TimestampMs::now())
                    .build();
                if task_connection
                    .send_notification(AcpMcpUpdateNotification(notification))
                    .is_err()
                {
                    break;
                }
                if change == McpSessionChange::Shutdown {
                    break;
                }
            }
            if let Ok(mut sessions) = watched.lock() {
                sessions.remove(&session_id);
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Installs or replaces one connection-level Session projection watcher.
    pub(crate) fn watch_projection(
        &self,
        session_id: &SessionId,
        connection: &ConnectionTo<Client>,
        watched: Arc<Mutex<BTreeMap<SessionId, Arc<CancellationToken>>>>,
        mut watch: ProjectionWatch,
    ) -> Result<(), agent_client_protocol::Error> {
        let cancellation = Arc::new(CancellationToken::new());
        {
            let mut watchers = watched.lock().map_err(|_poison_error| {
                agent_client_protocol::Error::into_internal_error(
                    std::io::Error::other("ACP Session watcher lock poisoned"),
                )
            })?;
            if let Some(previous) =
                watchers.insert(session_id.clone(), Arc::clone(&cancellation))
            {
                previous.cancel();
            }
        }
        for group in std::mem::take(&mut watch.backlog) {
            for notification in group
                .notifications(session_id, ProductIdentity::ACP_NAMESPACE)
                .map_err(agent_client_protocol::Error::into_internal_error)?
            {
                connection.send_notification(notification)?;
            }
            watch.record_sent(&group);
        }
        let projections = Arc::clone(&self.projections);
        let task_connection = connection.clone();
        let task_session_id = session_id.clone();
        let task_cancellation = Arc::clone(&cancellation);
        connection.spawn(async move {
            tracing::info!(
                "started ACP Session watcher for Session {}",
                task_session_id
            );
            let outcome: Result<(), agent_client_protocol::Error> = 'watch: loop {
                let received = tokio::select! {
                    () = task_cancellation.cancelled() => break Ok(()),
                    received = watch.receiver.recv() => received,
                };
                let groups = match received {
                    Ok(group) => vec![group],
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(
                        skipped,
                    )) => {
                        let Some(run_id) = &watch.run_id else {
                            tracing::warn!(
                                "stopped ACP Session watcher for Session {} after lagging by {} groups without a recovery cursor",
                                task_session_id,
                                skipped
                            );
                            break Err(agent_client_protocol::Error::into_internal_error(
                                std::io::Error::other(
                                    "ACP Session watcher lagged without a recovery cursor",
                                ),
                            ));
                        };
                        let Some(sequence) = watch.next_sequence else {
                            tracing::warn!(
                                "stopped ACP Session watcher for Session {} because lag recovery had no next sequence",
                                task_session_id
                            );
                            break Err(agent_client_protocol::Error::into_internal_error(
                                std::io::Error::other(
                                    "ACP Session watcher lag recovery has no next sequence",
                                ),
                            ));
                        };
                        match projections.snapshot_from(
                            &task_session_id,
                            run_id,
                            sequence,
                        ) {
                            Ok(groups) => groups,
                            Err(error) => {
                                tracing::warn!(
                                    "stopped ACP Session watcher for Session {} after lag recovery failed at sequence {}: {}",
                                    task_session_id,
                                    sequence,
                                    error
                                );
                                break Err(
                                    agent_client_protocol::Error::into_internal_error(error),
                                );
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break Ok(());
                    }
                };
                for group in groups {
                    if watch.next_sequence.is_some_and(|sequence| {
                        group.sequence < sequence
                    }) {
                        continue;
                    }
                    let notifications = group
                        .notifications(
                            &task_session_id,
                            ProductIdentity::ACP_NAMESPACE,
                        )
                        .map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )?;
                    for notification in notifications {
                        if let Err(error) =
                            task_connection.send_notification(notification)
                        {
                            tracing::warn!(
                                "stopped ACP Session watcher for Session {} after connection send failed: {}",
                                task_session_id,
                                error
                            );
                            break 'watch Ok(());
                        }
                    }
                    watch.record_sent(&group);
                }
            };
            if let Ok(mut watchers) = watched.lock()
                && watchers.get(&task_session_id).is_some_and(|current| {
                    Arc::ptr_eq(current, &task_cancellation)
                })
            {
                watchers.remove(&task_session_id);
            }
            tracing::info!(
                "stopped ACP Session watcher for Session {}",
                task_session_id
            );
            outcome
        })?;
        Ok(())
    }
}

/// Factory that creates an isolated ACP v2 connection over a shared kernel.
pub struct AcpServerFactory {
    kernel: Arc<Kernel>,
    id_generator: Arc<dyn IdGenerator>,
    runtime: AcpServerRuntime,
}

/// Shared projection state and immutable transport batching policy.
struct AcpServerRuntime {
    projections: Arc<ProjectionRegistry>,
    max_batch_size: NonZeroUsize,
}

/// Dependencies and watcher state shared by handlers on one ACP connection.
#[derive(typed_builder::TypedBuilder)]
struct ComponentContext {
    kernel: Arc<Kernel>,
    projections: Arc<ProjectionRegistry>,
    traces: AcpTraceFactory,
    watchers: ConnectionWatchers,
}

/// Mutable watcher collections shared by handlers on one ACP connection.
struct ConnectionWatchers {
    mcp: Arc<Mutex<BTreeSet<SessionId>>>,
    sessions: Arc<Mutex<BTreeMap<SessionId, Arc<CancellationToken>>>>,
}

impl AcpServerFactory {
    /// Creates an ACP server factory without client filesystem or terminal callbacks.
    #[must_use]
    pub fn new(
        kernel: Arc<Kernel>,
        id_generator: Arc<dyn IdGenerator>,
        max_batch_size: NonZeroUsize,
    ) -> Self {
        let projections =
            Arc::new(ProjectionRegistry::new(Arc::clone(&kernel)));
        Self {
            kernel,
            id_generator,
            runtime: AcpServerRuntime {
                projections,
                max_batch_size,
            },
        }
    }

    /// Builds one ACP v2 agent component for stdio, HTTP/SSE, or WebSocket transport.
    pub fn component(
        &self,
        transport: crate::AcpTransportKind,
    ) -> impl ConnectTo<Client> + 'static + use<> {
        // One connection context keeps handler captures compact while retaining
        // explicit Arc clones at the ownership boundaries that need them.
        let context = Arc::new(
            ComponentContext::builder()
                .kernel(Arc::clone(&self.kernel))
                .projections(Arc::clone(&self.runtime.projections))
                .traces(AcpTraceFactory::new(
                    Arc::clone(&self.id_generator),
                    transport,
                ))
                .watchers(ConnectionWatchers {
                    mcp: Arc::new(Mutex::new(BTreeSet::new())),
                    sessions: Arc::new(Mutex::new(BTreeMap::new())),
                })
                .build(),
        );

        BatchComponent::new(
            Agent.v2()
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::InitializeRequest,
                                responder: Responder<wire::InitializeResponse>,
                                _connection: ConnectionTo<Client>| {
                    let projections = Arc::clone(&context.projections);
                    let operation = context.traces.request(&responder, &request)?;
                    operation.run(async move {
                        let recovery = RecoveryRequest::from_meta(
                            request.meta.as_ref(),
                            ProductIdentity::ACP_NAMESPACE,
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?;
                        if recovery.as_ref().is_some_and(|recovery| {
                            recovery.version != RECOVERY_VERSION
                        }) {
                            return Err(
                                agent_client_protocol::Error::invalid_params()
                                    .data("unsupported session recovery version"),
                            );
                        }
                        let recovery_plans = recovery
                            .map(|recovery| {
                                recovery
                                    .sessions
                                    .iter()
                                    .map(|cursor| {
                                        projections.recovery_plan(cursor)
                                    })
                                    .collect::<Result<Vec<_>, _>>()
                            })
                            .transpose()
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?;
                        if let Some(plans) = &recovery_plans {
                            let watch_count = plans
                                .iter()
                                .filter(|plan| plan.mode == RecoveryMode::Watch)
                                .count();
                            tracing::info!(
                                "planned ACP Session recovery for {} Sessions: {} watch, {} full Resume",
                                plans.len(),
                                watch_count,
                                plans.len().saturating_sub(watch_count)
                            );
                        }
                        let methods = [
                        AcpExtensionMethod::FollowUp,
                        AcpExtensionMethod::Tree,
                        AcpExtensionMethod::Navigate,
                        AcpExtensionMethod::Branch,
                        AcpExtensionMethod::Fork,
                        AcpExtensionMethod::Compact,
                        AcpExtensionMethod::PendingMessages,
                        AcpExtensionMethod::PendingMessageRemove,
                        AcpExtensionMethod::ClearQueue,
                        AcpExtensionMethod::SessionRename,
                        AcpExtensionMethod::SessionRuntime,
                        AcpExtensionMethod::InvokeSkill,
                        AcpExtensionMethod::SkillList,
                        AcpExtensionMethod::McpStatus,
                        AcpExtensionMethod::McpReconnect,
                        AcpExtensionMethod::McpPromptGet,
                        AcpExtensionMethod::McpResourceRead,
                        AcpExtensionMethod::McpComplete,
                        AcpExtensionMethod::McpOAuthContinue,
                        AcpExtensionMethod::McpElicitationList,
                        AcpExtensionMethod::McpElicitationRespond,
                        AcpExtensionMethod::ExtensionCommand,
                        AcpExtensionMethod::UserBash,
                    ]
                    .into_iter()
                    .map(|method| serde_json::Value::String(method.to_string()))
                    .collect::<Vec<_>>();
                    let mut product_meta = serde_json::Map::from_iter([(
                        "methods".to_string(),
                        serde_json::Value::Array(methods),
                    )]);
                    if let Some(sessions) = recovery_plans {
                        product_meta.insert(
                            "sessionRecovery".to_string(),
                            serde_json::to_value(RecoveryResponse {
                                version: RECOVERY_VERSION,
                                sessions,
                            })
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?,
                        );
                    }
                    let capabilities_meta = wire::Meta::from_iter([(
                        ProductIdentity::ACP_NAMESPACE.to_string(),
                        serde_json::Value::Object(product_meta),
                    )]);
                        responder.respond(
                            wire::InitializeResponse::new(
                            ProtocolVersion::V2,
                            wire::Implementation::new(
                                ProductIdentity::SLUG,
                                env!("CARGO_PKG_VERSION"),
                            )
                            .title(ProductIdentity::NAME),
                        )
                        .capabilities(
                            wire::AgentCapabilities::new().session(
                                wire::SessionCapabilities::new()
                                    .delete(
                                        wire::SessionDeleteCapabilities::new(),
                                    )
                                    .prompt(
                                        wire::PromptCapabilities::new().image(
                                            wire::PromptImageCapabilities::new(),
                                        ),
                                    ),
                            ),
                        )
                            .meta(capabilities_meta),
                        )
                    }).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::NewSessionRequest,
                                responder: Responder<wire::NewSessionResponse>,
                                connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let projections = Arc::clone(&context.projections);
                    let mcp_watchers = Arc::clone(&context.watchers.mcp);
                    let session_watchers = Arc::clone(&context.watchers.sessions);
                    let operation = context.traces.request(&responder, &request)?;
                    operation.run(async move {
                        let cwd = AcpWorkingDirectory::try_from(
                            request.cwd.into_inner(),
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?;
                        let session_id = kernel
                            .create_generated_session(cwd.into_inner())
                            .await
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        let receiver = projections
                            .subscribe_live(&session_id)
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?;
                        let server = AcpServer::new(kernel, projections);
                        server.send_available_commands(&session_id, &connection)?;
                        server.watch_mcp(
                            &session_id,
                            &connection,
                            mcp_watchers,
                        )?;
                        server.watch_projection(
                            &session_id,
                            &connection,
                            session_watchers,
                            ProjectionWatch::builder()
                                .backlog(Vec::new())
                                .receiver(receiver)
                                .build(),
                        )?;
                        responder.respond(wire::NewSessionResponse::new(
                            session_id.to_string(),
                        ))
                    }).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::ListSessionsRequest,
                                responder: Responder<wire::ListSessionsResponse>,
                                _connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let operation = context.traces.request(&responder, &request)?;
                    operation.run(async move {
                        if request.cursor.is_some() {
                            return Err(agent_client_protocol::Error::invalid_params()
                                .data("session/list cursor is not recognized"));
                        }
                        let cwd = request
                            .cwd
                            .as_ref()
                            .map(|path| path.as_ref());
                        let sessions = kernel
                            .list_sessions(cwd)
                            .map_err(agent_client_protocol::Error::into_internal_error)?
                            .into_iter()
                            .map(|session| {
                                wire::SessionInfo::new(
                                    session.session_id.to_string(),
                                    session.cwd,
                                )
                                .title(session.name)
                                .meta(wire::Meta::from_iter([(
                                    ProductIdentity::ACP_NAMESPACE.to_string(),
                                    serde_json::json!({
                                        "createdAtMs": session.created_at_ms.to_string(),
                                        "modifiedAtMs": session.modified_at_ms.to_string(),
                                        "parentSessionId": session.parent_session_id
                                            .map(|id| id.to_string()),
                                    }),
                                )]))
                            })
                            .collect();
                        responder.respond(wire::ListSessionsResponse::new(sessions))
                    }).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::ResumeSessionRequest,
                                responder: Responder<wire::ResumeSessionResponse>,
                                connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let projections = Arc::clone(&context.projections);
                    let mcp_watchers = Arc::clone(&context.watchers.mcp);
                    let session_watchers = Arc::clone(&context.watchers.sessions);
                    let operation = context.traces.request(&responder, &request)?;
                    operation.run(async move {
                        let session_id = SessionId::try_from(request.session_id.to_string())
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?;
                        let replay = AcpReplayRequest::try_from(
                            request.replay_from,
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?;
                        let cwd = AcpWorkingDirectory::try_from(
                            request.cwd.into_inner(),
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?
                        .into_inner();
                        let use_retained_projection = if matches!(
                            &replay,
                            AcpReplayRequest::Start
                        ) && projections.has_journal(&session_id).map_err(
                            agent_client_protocol::Error::into_internal_error,
                        )? {
                            match kernel.validate_session_attachment(
                                &session_id,
                                &cwd,
                            ) {
                                Ok(_snapshot) => true,
                                Err(KernelError::SessionNotFound(_)) => {
                                    // A direct Kernel close can outlive the ACP
                                    // journal, so durable Resume must replace it.
                                    projections.remove(&session_id).map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )?;
                                    false
                                }
                                Err(error) => {
                                    return Err(
                                        agent_client_protocol::Error::into_internal_error(
                                            error,
                                        ),
                                    );
                                }
                            }
                        } else {
                            false
                        };
                        let server = AcpServer::new(
                            Arc::clone(&kernel),
                            Arc::clone(&projections),
                        );
                        let durable_replay_from_start =
                            matches!(&replay, AcpReplayRequest::Start);
                        let (watch, recovery_metadata) = match replay {
                            AcpReplayRequest::Event(cursor) => {
                                kernel
                                    .validate_session_attachment(
                                        &session_id,
                                        &cwd,
                                    )
                                    .map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )?;
                                let subscription = match projections
                                    .subscribe_from_cursor(
                                        &session_id,
                                        &cursor.run_id,
                                        cursor.sequence,
                                    ) {
                                    Ok(subscription) => subscription,
                                    Err(error) => {
                                        tracing::warn!(
                                            "failed incremental ACP Resume for Session {}, Run {}, sequence {}: {}",
                                            session_id,
                                            cursor.run_id,
                                            cursor.sequence,
                                            error
                                        );
                                        let data = CursorErrorData::builder()
                                            .session_id(session_id.clone())
                                            .run_id(cursor.run_id.clone())
                                            .sequence(cursor.sequence)
                                            .build();
                                        let data = serde_json::to_value(data)
                                            .map_err(
                                                agent_client_protocol::Error::into_internal_error,
                                            )?;
                                        return Err(
                                            agent_client_protocol::Error::invalid_params()
                                                .data(data),
                                        );
                                    }
                                };
                                let metadata = ResumeMeta::builder()
                                    .mode(RecoveryMode::Watch)
                                    .run_id(subscription.run_id.clone())
                                    .journal_tail_sequence(
                                        subscription.tail_sequence,
                                    )
                                    .running(subscription.running)
                                    .build();
                                (ProjectionWatch::from(subscription), metadata)
                            }
                            AcpReplayRequest::Start if use_retained_projection =>
                            {
                                let subscription = projections
                                    .subscribe_from_start(&session_id)
                                    .map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )?;
                                server.send_replay_items(
                                    &session_id,
                                    &connection,
                                    subscription.baseline.clone(),
                                )?;
                                let metadata = ResumeMeta::builder()
                                    .mode(RecoveryMode::Resume)
                                    .run_id(subscription.run_id.clone())
                                    .journal_tail_sequence(
                                        subscription.tail_sequence,
                                    )
                                    .running(subscription.running)
                                    .build();
                                (ProjectionWatch::from(subscription), metadata)
                            }
                            AcpReplayRequest::None | AcpReplayRequest::Start => {
                                // Subscribe before attaching the durable Session so a
                                // concurrent connection cannot publish RunStart into
                                // the resume/replay gap.
                                let live_receiver = projections
                                    .subscribe_live(&session_id)
                                    .map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )?;
                                Box::pin(kernel.resume_session(
                                    session_id.clone(),
                                    cwd,
                                ))
                                .await
                                .map_err(
                                    agent_client_protocol::Error::into_internal_error,
                                )?;
                                // Capture durable state before choosing a projection
                                // strategy. If a Run starts during this snapshot, its
                                // journal is selected exclusively and the snapshot is
                                // discarded, preventing duplicate persisted messages.
                                let durable_replay = if durable_replay_from_start {
                                    Some(kernel.session_replay(&session_id).map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )?)
                                } else {
                                    None
                                };
                                if let Some(durable_replay) = durable_replay {
                                    match projections.select_durable_resume(
                                        &session_id,
                                        live_receiver,
                                        durable_replay,
                                    ).map_err(
                                        agent_client_protocol::Error::into_internal_error,
                                    )? {
                                        DurableResumeSelection::Retained(subscription) => {
                                            server.send_replay_items(
                                                &session_id,
                                                &connection,
                                                subscription.baseline.clone(),
                                            )?;
                                            let metadata = ResumeMeta::builder()
                                                .mode(RecoveryMode::Resume)
                                                .run_id(subscription.run_id.clone())
                                                .journal_tail_sequence(
                                                    subscription.tail_sequence,
                                                )
                                                .running(subscription.running)
                                                .build();
                                            (ProjectionWatch::from(subscription), metadata)
                                        }
                                        DurableResumeSelection::Live { replay, receiver } => {
                                            server.send_replay_items(
                                                &session_id,
                                                &connection,
                                                replay,
                                            )?;
                                            (
                                                ProjectionWatch::builder()
                                                    .backlog(Vec::new())
                                                    .receiver(receiver)
                                                    .build(),
                                                ResumeMeta::builder()
                                                    .mode(RecoveryMode::Resume)
                                                    .running(false)
                                                    .build(),
                                            )
                                        }
                                    }
                                } else {
                                    (
                                        ProjectionWatch::builder()
                                            .backlog(Vec::new())
                                            .receiver(live_receiver)
                                            .build(),
                                        ResumeMeta::builder()
                                            .mode(RecoveryMode::Resume)
                                            .running(false)
                                            .build(),
                                    )
                                }
                            }
                        };
                        server.send_available_commands(&session_id, &connection)?;
                        server.watch_mcp(
                            &session_id,
                            &connection,
                            mcp_watchers,
                        )?;
                        server.watch_projection(
                            &session_id,
                            &connection,
                            session_watchers,
                            watch,
                        )?;
                        responder.respond(
                            wire::ResumeSessionResponse::new().meta(
                                wire::Meta::from_iter([(
                                    ProductIdentity::ACP_NAMESPACE.to_string(),
                                    serde_json::json!({
                                        "sessionRecovery": recovery_metadata
                                    }),
                                )]),
                            ),
                        )
                    }).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::CloseSessionRequest,
                                responder: Responder<wire::CloseSessionResponse>,
                                _connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let projections = Arc::clone(&context.projections);
                    let operation = context.traces.request(&responder, &request)?;
                    operation.run(async move {
                        let session_id = SessionId::try_from(request.session_id.to_string())
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?;
                        kernel
                            .close_session(&session_id)
                            .await
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        projections
                            .remove(&session_id)
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?;
                        responder.respond(wire::CloseSessionResponse::new())
                    }).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::DeleteSessionRequest,
                                responder: Responder<wire::DeleteSessionResponse>,
                                _connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let projections = Arc::clone(&context.projections);
                    let operation = context.traces.request(&responder, &request)?;
                    operation.run(async move {
                        let session_id =
                            SessionId::try_from(request.session_id.to_string())
                                .map_err(|error| {
                                    agent_client_protocol::Error::invalid_params()
                                        .data(error.to_string())
                                })?;
                        kernel
                            .delete_session(&session_id)
                            .await
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?;
                        projections
                            .remove(&session_id)
                            .map_err(
                                agent_client_protocol::Error::into_internal_error,
                            )?;
                        responder.respond(wire::DeleteSessionResponse::new())
                    }).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: wire::PromptRequest,
                                responder: Responder<wire::PromptResponse>,
                                _connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let projections = Arc::clone(&context.projections);
                    let operation = context.traces.request(&responder, &request)?;
                    let trace_id = operation.trace_id().clone();
                    operation.start();
                    let task_operation = operation.clone();
                    let result = operation.instrument(async move {
                        let session_id = SessionId::try_from(request.session_id.to_string())
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?;
                        let input = PromptInput::try_from(request.prompt)
                            .map_err(|error| {
                                agent_client_protocol::Error::invalid_params()
                                    .data(error.to_string())
                            })?
                            .into_inner();
                        responder.respond(wire::PromptResponse::new())?;

                        let task_session_id = session_id.clone();
                        // A Prompt Run belongs to the Kernel Session rather than
                        // the requesting socket. Detaching it lets reconnecting
                        // clients recover through runtime polling and replay.
                        tokio::spawn(async move {
                            let detached_session_id = task_session_id.clone();
                            let result: Result<
                                (),
                                agent_client_protocol::Error,
                            > = task_operation
                                .settle(Box::pin(async move {
                                    let sink = projections
                                        .sink(task_session_id.clone());
                                    if let Err(error) = kernel
                                        .run_traced(
                                            RunRequest {
                                                session_id: task_session_id
                                                    .clone(),
                                                input,
                                            },
                                            sink,
                                            trace_id,
                                        )
                                        .await
                                    {
                                        tracing::error!(
                                            "Kernel Run for session {} failed: {}",
                                            task_session_id,
                                            error
                                        );
                                        if let Err(fallback_error) = projections
                                            .publish_terminal_fallback(
                                                &task_session_id,
                                            )
                                        {
                                            tracing::error!(
                                                "failed to publish terminal ACP fallback for Session {}: {}",
                                                task_session_id,
                                                fallback_error
                                            );
                                        }
                                    }
                                    Ok(())
                                }))
                                .await;
                            if let Err(error) = result {
                                tracing::warn!(
                                    "detached ACP projection for session {} ended with error: {}",
                                    detached_session_id,
                                    error
                                );
                            }
                        });
                        Ok(())
                    }).await;
                    if let Err(error) = &result {
                        operation.fail(error);
                    }
                    result
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                {
                    let context = Arc::clone(&context);
                    async move |notification: wire::CancelSessionNotification,
                                _connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let operation = context.traces.notification(
                        notification.method(),
                        &notification,
                    )?;
                    operation.run(async move {
                        let session_id = SessionId::try_from(
                            notification.session_id.to_string(),
                        )
                        .map_err(|error| {
                            agent_client_protocol::Error::invalid_params()
                                .data(error.to_string())
                        })?;
                        kernel
                            .cancel_session(&session_id)
                            .map_err(agent_client_protocol::Error::into_internal_error)
                    }).await
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                {
                    let context = Arc::clone(&context);
                    async move |request: AcpExtensionRequest,
                                responder,
                                connection: ConnectionTo<Client>| {
                    let kernel = Arc::clone(&context.kernel);
                    let projections = Arc::clone(&context.projections);
                    let mcp_watchers = Arc::clone(&context.watchers.mcp);
                    let session_watchers = Arc::clone(&context.watchers.sessions);
                    let operation = context.traces.request(
                        &responder,
                        request.parameters(),
                    )?;
                    // The extension dispatcher covers every product method;
                    // boxing keeps that large state machine out of the ACP handler future.
                    operation.run(Box::pin(async move {
                        let response = AcpExtensionDispatcher::builder()
                            .kernel(kernel)
                            .connection(connection)
                            .mcp_watchers(mcp_watchers)
                            .projections(projections)
                            .session_watchers(session_watchers)
                            .build()
                        .execute(request)
                        .await?;
                        responder.respond(response)
                    })).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            ),
            self.runtime.max_batch_size,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::input::PromptInput;
    use crate::projection::EventGroup;
    use crate::recovery::OperationPhase;
    use agent_client_protocol::schema::v2 as wire;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use protocol::{ContentBlock, RunId, RunInput, Sequence};

    use super::ProjectionWatch;

    /// A live-only watcher adopts the first recoverable Run cursor it sends.
    #[test]
    fn live_only_watch_tracks_cursor_for_lag_recovery() {
        let (_sender, receiver) = tokio::sync::broadcast::channel(8);
        let mut watch = ProjectionWatch::builder()
            .backlog(Vec::new())
            .receiver(receiver)
            .build();
        let group = Arc::new(
            EventGroup::builder()
                .run_id(RunId::try_from("run-live").expect("Run id"))
                .sequence(Sequence::try_from(7_u64).expect("sequence"))
                .metadata(wire::Meta::default())
                .updates(Vec::new())
                .operation_phase(OperationPhase::Start)
                .build(),
        );

        watch.record_sent(&group);

        assert_eq!(watch.run_id.as_ref().map(RunId::as_str), Some("run-live"));
        assert_eq!(watch.next_sequence.map(Sequence::get), Some(8));

        let ungrouped = EventGroup::builder()
            .sequence(Sequence::try_from(8_u64).expect("ungrouped sequence"))
            .metadata(wire::Meta::default())
            .updates(Vec::new())
            .build();
        watch.record_sent(&ungrouped);

        assert_eq!(watch.run_id.as_ref().map(RunId::as_str), Some("run-live"));
        assert_eq!(watch.next_sequence.map(Sequence::get), Some(8));
    }

    /// Ordered ACP text and image blocks remain lossless at Kernel ingress.
    #[test]
    fn prompt_input_preserves_ordered_text_and_image_blocks() {
        let input = PromptInput::try_from(vec![
            wire::ContentBlock::Text(wire::TextContent::new("Read the marker")),
            wire::ContentBlock::Image(wire::ImageContent::new(
                "Q0xBVy03MzE5",
                "image/png",
            )),
        ])
        .expect("valid image prompt");

        assert_eq!(
            input.into_inner(),
            RunInput::Blocks(vec![
                ContentBlock::Text {
                    text: "Read the marker".to_string(),
                },
                ContentBlock::Image {
                    data: "Q0xBVy03MzE5".to_string(),
                    mime_type: "image/png".to_string(),
                },
            ])
        );
    }

    /// Invalid, unsupported, and undeclared ACP content is rejected.
    #[test]
    fn prompt_input_rejects_invalid_images() {
        for block in [
            wire::ImageContent::new("not-base64", "image/png"),
            wire::ImageContent::new("Q0xBVw==", "image/svg+xml"),
        ] {
            assert!(
                PromptInput::try_from(vec![wire::ContentBlock::Image(block)])
                    .is_err()
            );
        }

        assert!(
            PromptInput::try_from(
                (0..6)
                    .map(|_| {
                        wire::ContentBlock::Image(wire::ImageContent::new(
                            "Q0xBVw==",
                            "image/png",
                        ))
                    })
                    .collect::<Vec<_>>()
            )
            .is_err()
        );

        assert!(
            PromptInput::try_from(vec![wire::ContentBlock::Other(
                wire::OtherContentBlock::new(
                    "future_media",
                    Default::default(),
                ),
            )])
            .is_err()
        );
    }

    /// Encoded payloads that cannot fit the decoded limit are rejected before decoding.
    #[test]
    fn prompt_input_enforces_decoded_image_size_limits() {
        let invalid_but_oversized = "!".repeat(13_981_020);
        let error =
            match PromptInput::try_from(vec![wire::ContentBlock::Image(
                wire::ImageContent::new(invalid_but_oversized, "image/png"),
            )]) {
                Err(error) => error,
                Ok(_) => panic!("oversized invalid image was accepted"),
            };
        assert!(error.to_string().contains("each decoded image"));

        let oversized =
            BASE64_STANDARD.encode(vec![0_u8; 10 * 1024 * 1024 + 1]);
        let error =
            match PromptInput::try_from(vec![wire::ContentBlock::Image(
                wire::ImageContent::new(oversized, "image/png"),
            )]) {
                Err(error) => error,
                Ok(_) => panic!("oversized image was accepted"),
            };
        assert!(error.to_string().contains("each decoded image"));

        let seven_mebibytes =
            BASE64_STANDARD.encode(vec![0_u8; 7 * 1024 * 1024]);
        let error = match PromptInput::try_from(
            (0..3)
                .map(|_| {
                    wire::ContentBlock::Image(wire::ImageContent::new(
                        seven_mebibytes.clone(),
                        "image/png",
                    ))
                })
                .collect::<Vec<_>>(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("oversized image aggregate was accepted"),
        };
        assert!(error.to_string().contains("in total"));
    }
}
