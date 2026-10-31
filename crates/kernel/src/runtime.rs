mod admission;
mod attempt;
mod command;
mod compaction;
mod extension;
mod input;
mod insertion;
mod lifecycle;
mod mcp;
mod model_state;
mod ownership;
mod prompt;
mod queue;
mod resources;
mod retry;
mod run;
mod session;
mod settings;
mod settlement;
mod subagent;
mod submit;
mod tools;
mod transcript;
mod tree;
mod user_bash;

use extension::{KernelExtensionDiagnostics, SessionExtensions};
use insertion::WaiterCancellation;
use lifecycle::{ActiveRunContext, SessionExecution, SessionLifecycle};
use mcp::SessionMcpRuntime;
use model_state::SessionModelState;
use resources::SessionResources;
use run::RunInvocation;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use ::extension::{
    DynamicCommandRegistry, ExtensionFactory, ExtensionRegistrar,
    ExtensionRuntime,
};
use ::tools::{
    BashExecutionRequest, BashExecutor, ToolExecutionContext, ToolFactory,
    ToolRegistry,
};
use config::TurnLimit;
use futures::StreamExt;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AgentOutcome,
    BashExecutionMessage, CompactionData, CompactionDetails, CompactionPolicy,
    CompactionReason, CompactionResult, ContentBlock, EntryId, EventMetadata,
    ExtensionFlagDefinition, IdGenerator, IdKind, LaneId, McpSessionSnapshot,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ModelRequest,
    ModelStreamEvent, QueueId, RecordId, RetryPolicy, RunId, RunRequest,
    RunResult, SessionId, SessionSummary, SessionTitle, SessionTreeEntry,
    SessionTreeSnapshot, StopReason, ThinkingLevel, TimestampMs, ToolCall,
    ToolResult, TraceId, TurnId, TurnIdentity, TurnOutcome, TurnRecord,
    TurnTiming, UserBashInput, UserBashRequest, UserBashResult,
};
use store::{
    Clock, EntryKind, NewEntry, NewRecord, RecordKind, SessionCreateOptions,
    SessionForkOptions, SessionStore, StoreFactory,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::{
    EventSink, Model, ModelCatalog, ModelError, ModelFactory, SinkError,
};
pub use admission::{AdmissionTicket, PromptOutcome, PromptRequest};
use attempt::{
    AssistantAttempt, AssistantAttemptResult, AssistantAttemptSettlement,
};
pub use compaction::ContextUsageEstimate;
use compaction::{
    CompactionExecution, CompactionIdentity, CompactionPolicyExt,
    OverflowRecovery, SummaryGeneration, SummaryProtocol,
};
pub use insertion::{InsertionNotice, InsertionReceiver};
use queue::{PendingQueue, QueueCancellationReason};
use retry::RetryState;
use settlement::{RunCompletion, RunSettlement};
pub use submit::{AnswerOutcome, SubmissionAnswer, SubmitRequest, WhenBusy};
use tools::{SessionToolState, ToolBatch, ToolBatchResult};
use transcript::{
    SessionTranscript, TranscriptNavigationCommit, TranscriptNavigationSnapshot,
};

/// Errors surfaced by kernel construction, persistence, extensions, and Turns.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    /// Shared MCP connections can only be managed from their owner Session.
    #[error("MCP connections for session {0} are managed by its owner session")]
    SharedMcpManagement(SessionId),
    /// Owned creation requires its owner to be an initialized live Session.
    #[error("owner session is unavailable: {0}")]
    OwnerUnavailable(SessionId),
    /// Event routing is installed once so concurrent runs share one projection.
    #[error("session event sinks are already installed")]
    EventSinksInstalled,
    /// Model construction or streaming failed.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// Tool registry construction failed.
    #[error("tool subsystem failed: {0}")]
    Tool(#[from] ::tools::ToolError),
    /// Session persistence failed.
    #[error("store subsystem failed: {0}")]
    Store(#[from] store::StoreError),
    /// A failed durable setting left the current Store outcome uncertain.
    #[error(
        "session persistence is uncertain after a failed write; close and resume the session before continuing"
    )]
    SessionPersistenceUncertain,
    /// Extension construction or dispatch failed.
    #[error("extension subsystem failed: {0}")]
    Extension(#[from] ::extension::ExtensionError),
    /// Session-scoped MCP construction failed.
    #[error("MCP subsystem failed: {0}")]
    Mcp(#[from] ::mcp::McpError),
    /// Skill discovery or invocation failed.
    #[error("skill subsystem failed: {0}")]
    Skill(#[from] skill::SkillError),
    /// Prompt resource discovery or rendering failed.
    #[error("system prompt failed: {0}")]
    Prompt(#[from] ::prompt::PromptError),
    /// A live event consumer failed.
    #[error(transparent)]
    Sink(#[from] SinkError),
    /// A requested session has not been registered with this kernel.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    /// A session identifier was registered more than once.
    #[error("session already exists: {0}")]
    DuplicateSession(SessionId),
    /// A registered session has not completed startup and resource publication.
    #[error("session is starting")]
    SessionStarting,
    /// A closing session cannot accept another serialized operation.
    #[error("session is closing")]
    SessionClosing,
    /// A resume request supplied a different working directory for a live session.
    #[error(
        "session working directory mismatch for {session_id}: expected {expected:?}, received {received:?}"
    )]
    SessionCwdMismatch {
        session_id: SessionId,
        expected: PathBuf,
        received: PathBuf,
    },
    /// A protocol scalar generated by an injected component was invalid.
    #[error("invalid generated protocol value: {0}")]
    Protocol(String),
    /// An extension intentionally prevented an operation.
    #[error("extension blocked operation: {0}")]
    ExtensionBlocked(String),
    /// A run exceeded the configured safety bound on sequential Turns.
    #[error("run exceeded the maximum of {0} turns")]
    TurnLimit(usize),
    /// A shared runtime lock was poisoned by a failed task.
    #[error("kernel state lock was poisoned")]
    Poisoned,
    /// A requested extension command was not registered.
    #[error("extension command not found: {0}")]
    ExtensionCommandNotFound(String),
    /// A recognized Slash Command produced a stable domain rejection.
    #[error(transparent)]
    SlashCommandRejected(#[from] protocol::SlashCommandError),
    /// The elicitation was already answered, cancelled, or never pending.
    #[error("MCP elicitation is already resolved: {0}")]
    ElicitationResolved(String),
    /// The active branch has no model-visible history before its retention boundary.
    #[error("there is no context to compact")]
    NoContextToCompact,
    /// Direct commands execute immediately and cannot enter a run queue.
    #[error("Slash Command cannot be queued: /{name} ({command_source:?})")]
    SlashCommandCannotQueue {
        name: String,
        command_source: protocol::SlashCommandSource,
    },
    /// Queue insertion requires a run that has already become active.
    #[error("session is not running: {0}")]
    SessionNotRunning(SessionId),
    /// A queue identifier did not match any pending message.
    #[error("pending message not found: {0}")]
    PendingMessageNotFound(QueueId),
    /// A client prompt was cancelled or closed before its user message was inserted.
    #[error("prompt was cancelled before insertion")]
    PromptCancelled,
}

impl KernelError {
    /// Copies a pre-insertion failure for the protocol adapter, keeping variants it maps specially.
    pub(in crate::runtime) fn for_insertion(&self) -> Self {
        match self {
            Self::PromptCancelled => Self::PromptCancelled,
            Self::SessionPersistenceUncertain => {
                Self::SessionPersistenceUncertain
            }
            Self::SessionNotFound(session_id) => {
                Self::SessionNotFound(session_id.clone())
            }
            Self::SessionClosing => Self::SessionClosing,
            Self::ExtensionBlocked(reason) => {
                Self::ExtensionBlocked(reason.clone())
            }
            other => Self::Protocol(other.to_string()),
        }
    }

    /// Reports whether this error is cooperative cancellation rather than a failure.
    pub(in crate::runtime) fn is_cancellation(&self) -> bool {
        matches!(
            self,
            Self::PromptCancelled
                | Self::Model(ModelError::Cancelled)
                | Self::Tool(::tools::ToolError::Cancelled)
        )
    }
}

/// Dependency-injected builder for one modular kernel lifecycle.
#[derive(typed_builder::TypedBuilder)]
pub struct KernelFactory {
    model_factory: Arc<dyn ModelFactory>,
    tool_factory: Arc<dyn ToolFactory>,
    store_factory: Arc<dyn StoreFactory>,
    extension_factory: Arc<dyn ExtensionFactory>,
    clock: Arc<dyn Clock>,
    id_generator: Arc<dyn IdGenerator>,
    /// Creates immutable Session Prompt resource snapshots.
    prompt_factory: Arc<dyn ::prompt::PromptFactory>,
    /// Optional session-scoped MCP tool factory.
    #[builder(default)]
    mcp_factory: Option<Arc<dyn ::mcp::McpFactory>>,
    /// Optional pi-compatible skill discovery factory.
    #[builder(default)]
    skill_factory: Option<Arc<dyn skill::SkillFactory>>,
    /// Controls whether effective Skills are listed in System Prompts.
    #[builder(default = true)]
    include_skill_instructions: bool,
    /// Controls whether sequential Turns are unlimited or explicitly bounded.
    #[builder(default)]
    max_turns: TurnLimit,
    #[builder(default)]
    when_busy: WhenBusy,
    /// Controls how many recent user Turns remain verbatim after compaction.
    #[builder(default)]
    compaction_policy: CompactionPolicy,
    /// Controls transient Assistant retries across distinct Turns.
    #[builder(default)]
    retry_policy: RetryPolicy,
    /// A provider cannot force an unbounded server-requested retry wait.
    #[builder(default = config::ProviderRetryConfig::default().max_retry_delay_ms)]
    provider_retry_delay_limit_ms: u64,
}

impl KernelFactory {
    /// Resolves every factory once and returns a ready session-owning kernel.
    pub fn build(self) -> Result<Kernel, KernelError> {
        // Static declarations must be frozen before provider/model construction
        // because provider contributions participate in the immutable catalog.
        let static_extensions =
            Arc::new(self.extension_factory.static_registration()?);
        // Validate definitions once, but runtime model bindings are owned by individual Sessions.
        self.model_factory.create_catalog(&static_extensions)?;
        let tools = Arc::new(self.tool_factory.create()?);
        Ok(Kernel::builder()
            .model_factory(self.model_factory)
            .static_extensions(static_extensions)
            .tools(tools)
            .store_factory(self.store_factory)
            .extension_factory(self.extension_factory)
            .clock(self.clock)
            .id_generator(self.id_generator)
            .prompt_factory(self.prompt_factory)
            .mcp_factory(self.mcp_factory)
            .skill_factory(self.skill_factory)
            .include_skill_instructions(self.include_skill_instructions)
            .max_turns(self.max_turns)
            .when_busy(self.when_busy)
            .compaction_policy(self.compaction_policy)
            .retry_policy(self.retry_policy)
            .provider_retry_delay_limit_ms(self.provider_retry_delay_limit_ms)
            .sessions(Arc::new(RwLock::new(HashMap::new())))
            .pending_sessions(Arc::new(Mutex::new(HashMap::new())))
            .shutdown(CancellationToken::new())
            .build())
    }
}

/// Session-owning agent runtime that serializes runs per session.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct Kernel {
    #[builder(default)]
    event_sinks: Arc<OnceLock<Arc<dyn crate::SessionEventSinks>>>,
    /// Each Session builds independent runtime bindings over shared provider definitions.
    model_factory: Arc<dyn ModelFactory>,
    static_extensions: Arc<protocol::StaticExtensionRegistration>,
    tools: Arc<ToolRegistry>,
    store_factory: Arc<dyn StoreFactory>,
    extension_factory: Arc<dyn ExtensionFactory>,
    clock: Arc<dyn Clock>,
    id_generator: Arc<dyn IdGenerator>,
    prompt_factory: Arc<dyn ::prompt::PromptFactory>,
    #[builder(default)]
    mcp_factory: Option<Arc<dyn ::mcp::McpFactory>>,
    #[builder(default)]
    skill_factory: Option<Arc<dyn skill::SkillFactory>>,
    include_skill_instructions: bool,
    max_turns: TurnLimit,
    #[builder(default)]
    when_busy: WhenBusy,
    compaction_policy: CompactionPolicy,
    retry_policy: RetryPolicy,
    provider_retry_delay_limit_ms: u64,
    sessions: Arc<RwLock<HashMap<SessionId, Arc<Session>>>>,
    pending_sessions:
        Arc<Mutex<HashMap<SessionId, tokio::sync::watch::Sender<()>>>>,
    shutdown: CancellationToken,
}

impl Kernel {
    /// Executes one run while preserving per-session serialization.
    pub async fn run(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
    ) -> Result<RunResult, KernelError> {
        let trace_id = TraceId::try_from(self.id_generator.next(IdKind::Trace))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.run_traced(request, sink, trace_id, None).await
    }

    /// Executes one run under an ingress Trace that remains stable through Tool calls.
    ///
    /// `insertion` resolves once the user message is inserted, or with the error that ended the
    /// Run first, so protocol adapters can accept a prompt exactly at insertion.
    pub async fn run_traced(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
        trace_id: TraceId,
        insertion: Option<Arc<InsertionNotice>>,
    ) -> Result<RunResult, KernelError> {
        self.run_with_source(
            request,
            sink,
            RunInvocation::builder()
                .input_source(protocol::InputSource::Rpc)
                .trace_id(trace_id)
                .insertion(insertion)
                .build(),
        )
        .await
        .map(|(result, _answer)| result)
    }

    /// Waits until an extension or the embedding application requests shutdown.
    pub async fn wait_for_shutdown(&self) {
        self.shutdown.cancelled().await;
    }

    /// Invokes one discovered skill explicitly and returns its complete source.
    pub fn invoke_skill(
        &self,
        session_id: &SessionId,
        name: &str,
    ) -> Result<String, KernelError> {
        self.session(session_id)?
            .skill_catalog()?
            .ok_or_else(|| skill::SkillError::NotFound(name.to_string()))?
            .invoke(name)
            .map_err(KernelError::from)
    }

    /// Lists effective skill metadata without reading or returning skill bodies.
    pub fn skills(
        &self,
        session_id: &SessionId,
    ) -> Result<protocol::SkillListResult, KernelError> {
        Ok(self.session(session_id)?.skill_catalog()?.map_or_else(
            protocol::SkillListResult::default,
            |catalog| protocol::SkillListResult {
                skills: catalog.descriptors(),
                diagnostics: catalog.diagnostics().to_vec(),
            },
        ))
    }

    /// Returns the latest atomically published MCP status and catalog snapshot.
    pub fn mcp_status(
        &self,
        session_id: &SessionId,
    ) -> Result<McpSessionSnapshot, KernelError> {
        self.session(session_id)?.mcp_snapshot()
    }

    /// Subscribes ACP to lightweight MCP snapshot revision notifications.
    pub fn subscribe_mcp(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<broadcast::Receiver<::mcp::McpSessionEvent>>, KernelError>
    {
        self.session(session_id)?.subscribe_mcp_projection()
    }

    /// Explicitly reconnects one Session Server through its configured negotiation policy.
    pub async fn mcp_reconnect(
        &self,
        request: protocol::McpReconnectRequest,
    ) -> Result<McpSessionSnapshot, KernelError> {
        let session = self.session(&request.session_id)?;
        session.mcp.ensure_owned(&request.session_id)?;
        let mcp = session.mcp.as_ref().ok_or_else(|| {
            ::mcp::McpError::ServerNotFound(request.server_id.clone())
        })?;
        mcp.reconnect(&request.server_id).await?;
        session.mcp_snapshot()
    }

    /// Completes one pending MCP OAuth browser round and returns the latest snapshot.
    pub async fn mcp_continue_authorization(
        &self,
        request: protocol::McpAuthorizationContinueRequest,
    ) -> Result<McpSessionSnapshot, KernelError> {
        let session = self.session(&request.session_id)?;
        session.mcp.ensure_owned(&request.session_id)?;
        let server_id = request.server_id.clone();
        let mcp = session
            .mcp
            .as_ref()
            .ok_or(::mcp::McpError::ServerNotFound(server_id))?;
        mcp.continue_authorization(request).await?;
        session.mcp_snapshot()
    }

    /// Resolves one pending Turn-scoped MCP elicitation for its owning Session.
    pub async fn mcp_respond_elicitation(
        &self,
        request: protocol::McpElicitationResponseRequest,
    ) -> Result<(), KernelError> {
        let session = self.session(&request.session_id)?;
        session.resolve_mcp_elicitation(request).await
    }

    /// Retrieves one MCP Prompt through the owning Session runtime.
    pub async fn mcp_get_prompt(
        &self,
        request: protocol::McpSessionPromptRequest,
    ) -> Result<protocol::McpPromptResult, KernelError> {
        let session = self.session(&request.session_id)?;
        let mcp = session.mcp.as_ref().ok_or_else(|| {
            ::mcp::McpError::ServerNotFound(
                request.request.reference.server_id.clone(),
            )
        })?;
        Ok(mcp.get_prompt(request.request).await?)
    }

    /// Reads one MCP Resource through the owning Session runtime.
    pub async fn mcp_read_resource(
        &self,
        request: protocol::McpSessionResourceRequest,
        cancellation: &CancellationToken,
    ) -> Result<protocol::McpResourceResult, KernelError> {
        let session = self.session(&request.session_id)?;
        let mcp = session.mcp.as_ref().ok_or_else(|| {
            ::mcp::McpError::ServerNotFound(
                request.request.reference.server_id.clone(),
            )
        })?;
        Ok(mcp.read_resource(request.request, cancellation).await?)
    }

    /// Completes one MCP Prompt or Resource Template argument through its Session.
    pub async fn mcp_complete(
        &self,
        request: protocol::McpSessionCompletionRequest,
    ) -> Result<protocol::McpCompletionResult, KernelError> {
        let server_id = request.request.target.server_id().clone();
        let session = self.session(&request.session_id)?;
        let mcp = session
            .mcp
            .as_ref()
            .ok_or(::mcp::McpError::ServerNotFound(server_id))?;
        Ok(mcp.complete(request.request).await?)
    }

    /// Returns immutable command-line flag declarations from static extensions.
    #[must_use]
    pub fn extension_flags(&self) -> Vec<ExtensionFlagDefinition> {
        self.static_extensions.flags.clone()
    }

    /// Returns immutable parsed values for statically registered flags.
    #[must_use]
    pub fn extension_flag_values(
        &self,
    ) -> serde_json::Map<String, serde_json::Value> {
        self.static_extensions.flag_values.clone()
    }

    /// Dispatches one registered extension command and returns its typed directive as JSON.
    pub async fn invoke_extension_command(
        &self,
        session_id: &SessionId,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, KernelError> {
        let session = self.session(session_id)?;
        let command = session
            .extensions
            .command_snapshot()
            .map_err(|error| KernelError::ExtensionBlocked(error.to_string()))?
            .resolve(&name)
            .map_err(|_error| KernelError::ExtensionCommandNotFound(name))?;
        let context = self
            .extension_context(&session, None, None)?
            .for_extension(&command.extension_id);
        command
            .handler
            .handle(
                "",
                &arguments,
                &::extension::ExtensionCommandContext { event: context },
            )
            .await?;
        Ok(serde_json::Value::Null)
    }

    /// Generates and validates one transcript message identifier.
    fn message_id(&self) -> Result<MessageId, KernelError> {
        MessageId::try_from(self.id_generator.next(IdKind::Message))
            .map_err(|error| KernelError::Protocol(error.to_string()))
    }
}

impl From<serde_json::Error> for KernelError {
    /// Treats persistence JSON conversion as a store-boundary failure.
    fn from(error: serde_json::Error) -> Self {
        Self::Store(store::StoreError::Json(error))
    }
}

#[derive(typed_builder::TypedBuilder)]
struct Session {
    #[builder(default)]
    queue_publish: tokio::sync::Mutex<()>,
    #[builder(default)]
    queue_publishes: tokio_util::task::TaskTracker,
    #[builder(default)]
    owned_creation: tokio::sync::RwLock<()>,
    #[builder(default)]
    background: Arc<subagent::SessionBackgroundWork>,
    #[builder(default)]
    ownership: Option<protocol::SessionOwnership>,
    cwd: PathBuf,
    transcript: Arc<SessionTranscript>,
    mcp: SessionMcpRuntime,
    resources: OnceLock<SessionResources>,
    extensions: SessionExtensions,
    tools: Arc<SessionToolState>,
    model: SessionModelState,
    /// Serializes settings changes so reported snapshots follow change order.
    #[builder(default)]
    settings_changes: tokio::sync::Mutex<()>,
    execution: Arc<SessionExecution>,
}

/// Retains the request payload and its one-shot response route as one state entry.
struct PendingMcpElicitation {
    request: protocol::McpElicitationRequest,
    sender: tokio::sync::oneshot::Sender<protocol::McpElicitationResult>,
}

struct EventEmitter {
    clock: Arc<dyn Clock>,
    sink: Arc<dyn EventSink>,
    session: Arc<Session>,
}

impl EventEmitter {
    /// Emits one event using a fresh current timestamp from the injected clock.
    async fn emit(
        &self,
        turn_id: TurnId,
        payload: AgentEventPayload,
    ) -> Result<(), KernelError> {
        self.emit_at(turn_id, self.clock.now(), payload).await
    }

    /// Emits one event at an already captured output timestamp.
    async fn emit_at(
        &self,
        turn_id: TurnId,
        timestamp_ms: TimestampMs,
        payload: AgentEventPayload,
    ) -> Result<(), KernelError> {
        let sequence = self.session.execution.next_sequence()?;
        self.sink
            .emit(AgentEvent {
                metadata: EventMetadata {
                    turn_id,
                    timestamp_ms,
                    sequence,
                },
                payload,
            })
            .await?;
        Ok(())
    }
}
