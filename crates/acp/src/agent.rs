use std::{
    collections::HashMap,
    io::{BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use agent_client_protocol::schema as official_acp;
use agent_client_protocol::{
    Client as OfficialClient, ConnectTo, ConnectionTo, JsonRpcMessage, Responder,
};
use kernel::{
    AgentLoopConfig, SessionTaskContext, ThreadHandle, ThreadRunRequest, ThreadRuntime,
    events::{AgentEvent, EventSink, ToolCallCompletionStatus},
    model::AgentModel,
    session::{SessionId, ThreadId},
    tools::{
        ToolApprovalFuture, ToolApprovalHandler, ToolApprovalProfile, ToolApprovalRequest,
        router::ToolRouter,
    },
};
use serde_json::{Value, json};
use snafu::{ResultExt, Snafu};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, warn};

use crate::{
    message::{AcpMessage, SessionRpc},
    permission::{build_tool_permission_request, permission_response_approved},
};

pub type SharedAcpWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Errors produced by the ACP stdio adapter while parsing JSON-RPC or running turns.
#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("ACP JSON handling failed on `{stage}`, {source}"))]
    Json {
        source: serde_json::Error,
        stage: String,
    },

    #[snafu(display("ACP I/O failed on `{stage}`, {source}"))]
    Io {
        source: std::io::Error,
        stage: String,
    },

    #[snafu(display("ACP request is invalid on `{stage}`: {message}"))]
    InvalidRequest { message: String, stage: String },

    #[snafu(display("ACP method is not supported on `{stage}`: {method}"))]
    MethodNotFound { method: String, stage: String },

    #[snafu(display("ACP runtime failed on `{stage}`, {source}"))]
    Kernel {
        #[snafu(source(from(kernel::Error, Box::new)))]
        source: Box<kernel::Error>,
        stage: String,
    },

    #[snafu(display("ACP official error on `{stage}`, {source}"))]
    OfficialAcp {
        source: official_acp::Error,
        stage: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<Error> for official_acp::Error {
    /// Converts adapter errors into official ACP errors for typed SDK responders.
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::OfficialAcp { source, .. } => source,
            Error::Json { .. } => official_acp::Error::parse_error().data(message),
            Error::InvalidRequest { .. } => official_acp::Error::invalid_params().data(message),
            Error::MethodNotFound { .. } => official_acp::Error::method_not_found().data(message),
            Error::Io { .. } | Error::Kernel { .. } => {
                official_acp::Error::internal_error().data(message)
            }
        }
    }
}

impl Error {
    /// Returns the JSON-RPC error code that corresponds to this adapter error.
    fn rpc_code(&self) -> i32 {
        match self {
            Error::Json { .. } => -32700,
            Error::InvalidRequest { .. } => -32602,
            Error::MethodNotFound { .. } => -32601,
            Error::Io { .. } | Error::Kernel { .. } | Error::OfficialAcp { .. } => -32603,
        }
    }

    /// Converts this adapter error into a JSON-RPC error object.
    fn to_json_rpc_error(&self) -> Value {
        json!({
            "code": self.rpc_code(),
            "message": self.to_string(),
        })
    }
}

/// Minimal JSON-RPC request envelope accepted by the ACP stdio transport.
#[derive(Debug, serde::Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

/// In-memory session state held for one ACP connection.
#[derive(Clone)]
struct AcpSession {
    thread: ThreadHandle,
    router: Arc<ToolRouter>,
}

/// ACP stdio agent that exposes the existing kernel runtime through JSON-RPC methods.
pub struct AcpAgent<M> {
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    /// Indicates the active router already exposes ACP filesystem tools for the human client path.
    has_client_fs_tools: bool,
    skills: skills::SkillConfig,
    tool_approval_profile: ToolApprovalProfile,
    tool_approval_profile_configured: bool,
    tool_approval_handler: Option<ToolApprovalHandler>,
    writer: SharedAcpWriter,
    sessions: HashMap<String, AcpSession>,
}

impl<M> AcpAgent<M>
where
    M: AgentModel + 'static,
{
    /// Builds an ACP agent bound to the shared runtime dependencies used by CLI modes.
    pub fn new(
        model: Arc<M>,
        store: Arc<SessionTaskContext>,
        router: Arc<ToolRouter>,
        skills: skills::SkillConfig,
        writer: SharedAcpWriter,
        has_client_fs_tools: bool,
    ) -> Self {
        Self {
            model,
            store,
            router,
            has_client_fs_tools,
            skills,
            tool_approval_profile: ToolApprovalProfile::TrustAll,
            tool_approval_profile_configured: false,
            tool_approval_handler: None,
            writer,
            sessions: HashMap::new(),
        }
    }

    /// Installs a tool approval handler used for tools marked as requiring approval.
    pub fn with_tool_approval_handler(mut self, handler: ToolApprovalHandler) -> Self {
        self.tool_approval_handler = Some(handler);
        self
    }

    /// Selects the tool approval profile used for prompt turns.
    pub fn with_tool_approval_profile(
        mut self,
        tool_approval_profile: ToolApprovalProfile,
    ) -> Self {
        self.tool_approval_profile = tool_approval_profile;
        self.tool_approval_profile_configured = true;
        self
    }

    /// Runs this agent through the official ACP SDK transport and typed dispatch layer.
    pub async fn connect_sdk(
        self,
        transport: impl ConnectTo<agent_client_protocol::Agent> + 'static,
    ) -> Result<()> {
        let state = Arc::new(AsyncMutex::new(self));
        let session_new_state = Arc::clone(&state);
        let prompt_state = Arc::clone(&state);

        agent_client_protocol::Agent
            .builder()
            .name("clawcode")
            .on_receive_request(
                async move |initialize: official_acp::InitializeRequest,
                            responder: Responder<official_acp::InitializeResponse>,
                            _connection: ConnectionTo<OfficialClient>| {
                    responder.respond(initialize_response(&initialize))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: official_acp::NewSessionRequest,
                            responder: Responder<official_acp::NewSessionResponse>,
                            _connection: ConnectionTo<OfficialClient>| {
                    let mut agent = session_new_state.lock().await;
                    match agent.handle_session_new_response(request).await {
                        Ok(response) => responder.respond(response),
                        Err(error) => responder.respond_with_error(error.into()),
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: official_acp::PromptRequest,
                            responder: Responder<official_acp::PromptResponse>,
                            connection: ConnectionTo<OfficialClient>| {
                    let prompt_state = Arc::clone(&prompt_state);
                    let prompt_connection = connection.clone();
                    // Prompt turns can send client requests such as `session/request_permission`.
                    // Run the turn outside the SDK dispatch task so those responses can be read.
                    connection.spawn(async move {
                        let response =
                            spawn_sdk_prompt_turn(prompt_state, request, prompt_connection)
                                .await
                                .map_err(|_| Error::OfficialAcp {
                                    source: official_acp::Error::internal_error()
                                        .data("SDK prompt worker dropped its response"),
                                    stage: "acp-sdk-prompt-worker".to_string(),
                                });
                        match response {
                            Ok(Ok(response)) => responder.respond(response),
                            Ok(Err(error)) | Err(error) => {
                                responder.respond_with_error(error.into())
                            }
                        }
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(transport)
            .await
            .context(OfficialAcpSnafu {
                stage: "acp-sdk-connect".to_string(),
            })
    }

    /// Handles one newline-delimited JSON-RPC message from the ACP stdio transport.
    pub async fn handle_line(&mut self, line: &str) -> Result<()> {
        info!(direction = "recv", payload = %line, "acp protocol message");
        let parsed = serde_json::from_str::<JsonRpcRequest>(line).context(JsonSnafu {
            stage: "acp-parse-json-rpc".to_string(),
        });
        let request = match parsed {
            Ok(request) => request,
            Err(error) => {
                warn!(direction = "recv", error = %error, "acp protocol parse failed");
                self.write_response(SessionRpc::error(Value::Null, error.to_json_rpc_error()))?;
                return Ok(());
            }
        };

        debug!(id = ?request.id, method = %request.method, "acp request parsed");
        let id = request.id.clone();
        let result = self.dispatch(request).await;

        // JSON-RPC notifications have no id and therefore do not receive a response.
        if let Some(id) = id {
            let response = match result {
                Ok(result) => SessionRpc::response(id, result),
                Err(error) => SessionRpc::error(id, error.to_json_rpc_error()),
            };
            info!(direction = "send", payload = %response, "acp protocol message");
            self.write_response(response)?;
        }

        Ok(())
    }

    /// Routes a parsed JSON-RPC request to the matching ACP method handler.
    async fn dispatch(&mut self, request: JsonRpcRequest) -> Result<Value> {
        let method = request.method;
        let params = request.params.unwrap_or(Value::Null);
        let request =
            official_acp::ClientRequest::parse_message(&method, &params).map_err(|error| {
                acp_schema_parse_error(&method, error, "acp-dispatch-typed-request")
            })?;

        match request {
            official_acp::ClientRequest::InitializeRequest(params) => {
                Ok(initialize_result(&params))
            }
            official_acp::ClientRequest::NewSessionRequest(params) => {
                self.handle_session_new(params).await
            }
            official_acp::ClientRequest::PromptRequest(params) => {
                self.handle_session_prompt(params).await
            }
            _ => MethodNotFoundSnafu {
                method,
                stage: "acp-dispatch-method".to_string(),
            }
            .fail(),
        }
    }

    /// Creates a new kernel thread for an ACP session and returns its session id.
    async fn handle_session_new(
        &mut self,
        params: official_acp::NewSessionRequest,
    ) -> Result<Value> {
        Ok(SessionRpc::value(
            &self.handle_session_new_response(params).await?,
        ))
    }

    /// Creates a new kernel thread for an ACP session and returns the typed ACP response.
    async fn handle_session_new_response(
        &mut self,
        params: official_acp::NewSessionRequest,
    ) -> Result<official_acp::NewSessionResponse> {
        if !params.cwd.is_absolute() {
            return InvalidRequestSnafu {
                message: "session/new cwd must be absolute".to_string(),
                stage: "acp-session-new-cwd-absolute".to_string(),
            }
            .fail();
        }
        let session_router = self.build_session_router(params.cwd.clone()).await?;

        let session_id = SessionId::new();
        let thread =
            ThreadHandle::new(session_id.clone(), ThreadId::new()).with_cwd(params.cwd.clone());
        let session_id = session_id.to_string();
        self.sessions.insert(
            session_id.clone(),
            AcpSession {
                thread,
                router: session_router,
            },
        );

        Ok(official_acp::NewSessionResponse::new(session_id))
    }

    /// Builds a per-session tool router rooted at the requested workspace root.
    async fn build_session_router(&self, cwd: PathBuf) -> Result<Arc<ToolRouter>> {
        // Human ACP sessions carry ACP-backed fs tools and already resolve paths through the client
        // renderer, so keep that router unchanged.
        if self.has_client_fs_tools {
            return Ok(Arc::clone(&self.router));
        }

        // For stdio server sessions (or any non-client wrapped router), rebuild tools against the
        // supplied session cwd so exec/patch/read/write operations are rooted correctly.
        Ok(Arc::new(ToolRouter::from_path(cwd).await))
    }

    /// Runs one ACP prompt turn through the kernel runtime and streams `session/update` events.
    async fn handle_session_prompt(
        &mut self,
        params: official_acp::PromptRequest,
    ) -> Result<Value> {
        Ok(SessionRpc::value(
            &self
                .handle_session_prompt_response_with_writer(params)
                .await?,
        ))
    }

    /// Runs one prompt turn for the manual transport and returns the typed ACP response.
    async fn handle_session_prompt_response_with_writer(
        &mut self,
        params: official_acp::PromptRequest,
    ) -> Result<official_acp::PromptResponse> {
        let session_id = params.session_id.to_string();
        let (session, prompt) = self.prepare_session_prompt(params)?;
        self.write_session_update(&session_id, AcpMessage::user_text(&prompt))?;

        let sink = AcpEventSink::new(session_id, Arc::clone(&self.writer));
        self.run_session_prompt(&session, prompt, sink, self.tool_approval_handler.clone())
            .await?;

        Ok(official_acp::PromptResponse::new(
            official_acp::StopReason::EndTurn,
        ))
    }

    /// Runs one prompt turn for the SDK transport and returns the typed ACP response.
    async fn handle_session_prompt_response(
        &mut self,
        params: official_acp::PromptRequest,
        connection: ConnectionTo<OfficialClient>,
    ) -> Result<official_acp::PromptResponse> {
        let session_id = params.session_id.to_string();
        let (session, prompt) = self.prepare_session_prompt(params)?;
        send_sdk_session_update(&connection, &session_id, AcpMessage::user_text(&prompt))?;

        let approval_handler = sdk_approval_handler(connection.clone());
        let sink = SdkAcpEventSink::new(session_id, connection);
        self.run_session_prompt(&session, prompt, sink, Some(approval_handler))
            .await?;

        Ok(official_acp::PromptResponse::new(
            official_acp::StopReason::EndTurn,
        ))
    }

    /// Normalizes a prompt request and looks up the session it targets.
    fn prepare_session_prompt(
        &self,
        params: official_acp::PromptRequest,
    ) -> Result<(AcpSession, String)> {
        let session_id = params.session_id.to_string();
        let prompt = KernelPromptText::try_from(params)
            .map(KernelPromptText::into_inner)
            .map_err(|message| Error::InvalidRequest {
                message,
                stage: "acp-session-prompt-content".to_string(),
            })?;
        let session =
            self.sessions
                .get(&session_id)
                .cloned()
                .ok_or_else(|| Error::InvalidRequest {
                    message: format!("unknown sessionId `{session_id}`"),
                    stage: "acp-session-prompt-lookup".to_string(),
                })?;

        Ok((session, prompt))
    }

    /// Executes a normalized prompt against the kernel runtime with the provided event sink.
    async fn run_session_prompt<E>(
        &self,
        session: &AcpSession,
        prompt: String,
        sink: E,
        tool_approval_handler: Option<ToolApprovalHandler>,
    ) -> Result<()>
    where
        E: EventSink + 'static,
    {
        let tool_approval_profile =
            if tool_approval_handler.is_some() && !self.tool_approval_profile_configured {
                // A handler means this ACP agent can ask the client. Without an explicit profile,
                // use tool metadata instead of silently bypassing approval through TrustAll.
                ToolApprovalProfile::Default
            } else {
                self.tool_approval_profile
            };

        let runtime = ThreadRuntime::new(
            Arc::clone(&self.model),
            Arc::clone(&self.store),
            Arc::clone(&session.router),
            Arc::new(sink),
        )
        .with_config(AgentLoopConfig {
            skills: self.skills.clone(),
            // ACP itself does not impose a prompt-turn request budget here; tool limits still come
            // from the kernel defaults unless configured elsewhere.
            max_iterations: usize::MAX,
            tool_approval_profile,
            tool_approval_handler,
            ..AgentLoopConfig::default()
        });

        runtime
            .run(&session.thread, ThreadRunRequest::new(prompt))
            .await
            .context(KernelSnafu {
                stage: "acp-session-prompt-run".to_string(),
            })?;

        Ok(())
    }

    /// Writes one JSON-RPC response or notification as a newline-delimited ACP message.
    fn write_response(&self, message: Value) -> Result<()> {
        write_json_line(&self.writer, &message)
    }

    /// Writes one `session/update` notification for the active ACP session.
    fn write_session_update(
        &self,
        session_id: &str,
        update: official_acp::SessionUpdate,
    ) -> Result<()> {
        let message = SessionRpc::session_update(session_id, update);
        info!(direction = "send", payload = %message, "acp protocol message");
        write_json_line(&self.writer, &message)
    }
}

/// Event sink that translates kernel runtime events into ACP `session/update` notifications.
struct AcpEventSink {
    session_id: String,
    writer: SharedAcpWriter,
}

impl AcpEventSink {
    /// Builds a sink for one ACP prompt turn.
    fn new(session_id: String, writer: SharedAcpWriter) -> Self {
        Self { session_id, writer }
    }

    /// Best-effort writes one ACP session update from an event callback.
    fn publish_update(&self, update: official_acp::SessionUpdate) {
        let message = SessionRpc::session_update(&self.session_id, update);
        info!(direction = "send", payload = %message, "acp protocol message");
        let _ = write_json_line(&self.writer, &message);
    }
}

#[async_trait::async_trait]
impl EventSink for AcpEventSink {
    /// Publishes model, reasoning, and tool progress using ACP session updates.
    async fn publish(&self, event: AgentEvent) {
        if let Ok(update) = AcpSessionUpdate::try_from(event) {
            self.publish_update(update.into());
        }
    }
}

/// Event sink that publishes kernel runtime updates through the official SDK connection.
struct SdkAcpEventSink {
    session_id: String,
    connection: ConnectionTo<OfficialClient>,
}

impl SdkAcpEventSink {
    /// Builds a sink for one SDK-backed ACP prompt turn.
    fn new(session_id: String, connection: ConnectionTo<OfficialClient>) -> Self {
        Self {
            session_id,
            connection,
        }
    }

    /// Best-effort writes one ACP session update through the SDK connection.
    fn publish_update(&self, update: official_acp::SessionUpdate) {
        let _ = send_sdk_session_update(&self.connection, &self.session_id, update);
    }
}

#[async_trait::async_trait]
impl EventSink for SdkAcpEventSink {
    /// Publishes model, reasoning, and tool progress through SDK `session/update` notifications.
    async fn publish(&self, event: AgentEvent) {
        if let Ok(update) = AcpSessionUpdate::try_from(event) {
            self.publish_update(update.into());
        }
    }
}

/// ACP session update produced from a kernel event.
struct AcpSessionUpdate(official_acp::SessionUpdate);

impl From<AcpSessionUpdate> for official_acp::SessionUpdate {
    /// Unwraps the official ACP session update from the local conversion wrapper.
    fn from(value: AcpSessionUpdate) -> Self {
        value.0
    }
}

/// Error returned when a kernel event has no ACP session-update representation.
struct IgnoredAgentEvent;

impl TryFrom<AgentEvent> for AcpSessionUpdate {
    type Error = IgnoredAgentEvent;

    /// Converts kernel runtime events into ACP session updates shared by all transports.
    fn try_from(event: AgentEvent) -> std::result::Result<Self, Self::Error> {
        let update = match event {
            AgentEvent::ModelTextDelta { text, .. } => AcpMessage::agent_text(text),
            AgentEvent::ModelReasoningContentDelta { text, .. } => AcpMessage::thought_text(text),
            AgentEvent::ToolCallRequested {
                name,
                handle_id,
                arguments,
                ..
            } => AcpMessage::tool_started(handle_id, name, arguments),
            AgentEvent::ToolCallCompleted {
                status,
                name,
                handle_id,
                output,
                structured_output,
                ..
            } => match status {
                ToolCallCompletionStatus::Succeeded => {
                    AcpMessage::tool_completed(handle_id, name, output, structured_output)
                }
                ToolCallCompletionStatus::Failed => {
                    AcpMessage::tool_failed(handle_id, name, output, structured_output)
                }
            },
            _ => return Err(IgnoredAgentEvent),
        };

        Ok(Self(update))
    }
}

/// Runs a non-`Send` kernel prompt future on a dedicated current-thread Tokio runtime.
async fn spawn_sdk_prompt_turn<M>(
    state: Arc<AsyncMutex<AcpAgent<M>>>,
    request: official_acp::PromptRequest,
    connection: ConnectionTo<OfficialClient>,
) -> std::result::Result<Result<official_acp::PromptResponse>, tokio::sync::oneshot::error::RecvError>
where
    M: AgentModel + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = run_sdk_prompt_turn_on_thread(state, request, connection);
        let _ = tx.send(result);
    });
    rx.await
}

/// Builds the per-prompt runtime used by SDK handlers and executes the kernel turn.
fn run_sdk_prompt_turn_on_thread<M>(
    state: Arc<AsyncMutex<AcpAgent<M>>>,
    request: official_acp::PromptRequest,
    connection: ConnectionTo<OfficialClient>,
) -> Result<official_acp::PromptResponse>
where
    M: AgentModel + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context(IoSnafu {
            stage: "acp-sdk-prompt-runtime".to_string(),
        })?;
    let mut agent = runtime.block_on(state.lock());
    runtime.block_on(agent.handle_session_prompt_response(request, connection))
}

/// Runs an ACP stdio agent over newline-delimited JSON-RPC input and output streams.
pub async fn run_stdio_agent<M, R>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    skills: skills::SkillConfig,
    input: R,
    writer: SharedAcpWriter,
) -> Result<()>
where
    M: AgentModel + 'static,
    R: BufRead,
{
    let mut agent = AcpAgent::new(model, store, router, skills, writer, false);

    for line in input.lines() {
        let line = line.context(IoSnafu {
            stage: "acp-read-line".to_string(),
        })?;
        if line.trim().is_empty() {
            continue;
        }
        debug!(bytes = line.len(), "acp stdio line received");
        agent.handle_line(&line).await?;
    }

    Ok(())
}

/// Runs an ACP stdio agent using the official SDK byte-stream transport.
pub async fn run_sdk_stdio_agent<M>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    skills: skills::SkillConfig,
    tool_approval_profile: ToolApprovalProfile,
) -> Result<()>
where
    M: AgentModel + 'static,
{
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let agent = AcpAgent::new(
        model,
        store,
        router,
        skills,
        shared_writer(std::io::sink()),
        false,
    )
    .with_tool_approval_profile(tool_approval_profile);
    agent
        .connect_sdk(agent_client_protocol::ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .await
}

/// Wraps a writer in the shared box used by the ACP adapter and event sink.
pub fn shared_writer<W>(writer: W) -> SharedAcpWriter
where
    W: Write + Send + 'static,
{
    Arc::new(Mutex::new(Box::new(writer)))
}

/// Builds the ACP initialize result, negotiating the single protocol version currently supported.
fn initialize_result(params: &official_acp::InitializeRequest) -> Value {
    SessionRpc::value(&initialize_response(params))
}

/// Builds the typed ACP initialize response advertised by both transport implementations.
fn initialize_response(
    _params: &official_acp::InitializeRequest,
) -> official_acp::InitializeResponse {
    let agent_capabilities = official_acp::AgentCapabilities::new()
        .load_session(false)
        .prompt_capabilities(
            official_acp::PromptCapabilities::new()
                .image(false)
                .audio(false)
                .embedded_context(true),
        )
        .mcp_capabilities(official_acp::McpCapabilities::new().http(false).sse(false));

    official_acp::InitializeResponse::new(official_acp::ProtocolVersion::V1)
        .agent_capabilities(agent_capabilities)
        .agent_info(
            official_acp::Implementation::new("clawcode", env!("CARGO_PKG_VERSION"))
                .title("ClawCode"),
        )
}

/// ACP SDK-backed approval handler that forwards tool approvals to the connected client.
struct SdkApprovalHandler {
    connection: ConnectionTo<OfficialClient>,
}

impl kernel::tools::ToolApproval for SdkApprovalHandler {
    /// Sends one ACP `session/request_permission` request and fails closed on transport errors.
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture {
        let connection = self.connection.clone();
        Box::pin(async move {
            let approval_request = build_tool_permission_request(&request);
            // ACP has no separate capability flag for `session/request_permission`; unsupported
            // clients fail closed here so high-risk tool calls are never silently allowed.
            match connection.send_request(approval_request).block_task().await {
                Ok(response) => permission_response_approved(&response),
                Err(error) => {
                    warn!(error = %error, "ACP permission request failed");
                    false
                }
            }
        })
    }
}

/// Builds an async ACP permission handler that forwards approval requests to the connected client.
fn sdk_approval_handler(connection: ConnectionTo<OfficialClient>) -> ToolApprovalHandler {
    Arc::new(SdkApprovalHandler { connection })
}

/// Sends one typed `session/update` notification through an official SDK connection.
fn send_sdk_session_update(
    connection: &ConnectionTo<OfficialClient>,
    session_id: &str,
    update: official_acp::SessionUpdate,
) -> Result<()> {
    let message = SessionRpc::session_update(session_id, update.clone());
    info!(direction = "send", payload = %message, "acp protocol message");
    connection
        .send_notification(official_acp::SessionNotification::new(
            session_id.to_string(),
            update,
        ))
        .context(OfficialAcpSnafu {
            stage: "acp-sdk-session-update".to_string(),
        })
}

/// Converts SDK schema parsing errors into this adapter's SNAFU error type.
fn acp_schema_parse_error(method: &str, error: agent_client_protocol::Error, stage: &str) -> Error {
    let code = SessionRpc::value(&error.code)
        .as_i64()
        .and_then(|code| i32::try_from(code).ok());
    if code == Some(-32601) {
        Error::MethodNotFound {
            method: method.to_string(),
            stage: stage.to_string(),
        }
    } else {
        Error::InvalidRequest {
            message: error.message,
            stage: stage.to_string(),
        }
    }
}

/// Text prompt accepted by the existing kernel runtime after ACP content normalization.
struct KernelPromptText(String);

impl KernelPromptText {
    /// Returns the normalized prompt text for kernel execution.
    fn into_inner(self) -> String {
        self.0
    }
}

impl TryFrom<official_acp::PromptRequest> for KernelPromptText {
    type Error = String;

    /// Converts an official ACP prompt request into the text-only prompt used by the kernel.
    fn try_from(value: official_acp::PromptRequest) -> std::result::Result<Self, Self::Error> {
        Self::try_from(value.prompt)
    }
}

impl TryFrom<Vec<official_acp::ContentBlock>> for KernelPromptText {
    type Error = String;

    /// Converts official ACP prompt content blocks into the text-only input currently accepted by the kernel.
    fn try_from(blocks: Vec<official_acp::ContentBlock>) -> std::result::Result<Self, Self::Error> {
        let mut text_parts = Vec::new();

        for block in blocks {
            match block {
                official_acp::ContentBlock::Text(text) => {
                    text_parts.push(text.text);
                }
                official_acp::ContentBlock::Resource(resource) => {
                    if let official_acp::EmbeddedResourceResource::TextResourceContents(resource) =
                        resource.resource
                    {
                        text_parts.push(resource.text);
                    }
                }
                official_acp::ContentBlock::ResourceLink(resource_link) => {
                    // Resource links are preserved as explicit text references until client file APIs exist.
                    text_parts.push(format!("[resource_link] {}", resource_link.uri));
                }
                official_acp::ContentBlock::Image(_) => {
                    return Err("unsupported prompt content block type `image`".to_string());
                }
                official_acp::ContentBlock::Audio(_) => {
                    return Err("unsupported prompt content block type `audio`".to_string());
                }
                _ => {
                    return Err(
                        "unsupported prompt content block type from future ACP schema".to_string(),
                    );
                }
            }
        }

        let prompt = text_parts.join("\n\n");
        if prompt.trim().is_empty() {
            Err("prompt content must not be empty".to_string())
        } else {
            Ok(Self(prompt))
        }
    }
}

/// Writes one JSON object followed by a newline as required by ACP stdio.
fn write_json_line(writer: &SharedAcpWriter, message: &Value) -> Result<()> {
    let mut writer = writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    serde_json::to_writer(&mut *writer, message).context(JsonSnafu {
        stage: "acp-write-json".to_string(),
    })?;
    writeln!(writer).context(IoSnafu {
        stage: "acp-write-newline".to_string(),
    })?;
    writer.flush().context(IoSnafu {
        stage: "acp-flush-message".to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io,
        sync::{Arc, Mutex as StdMutex},
    };

    use agent_client_protocol::{JsonRpcResponse, SentRequest};
    use kernel::{
        Result as KernelResult,
        model::{ModelRequest, ModelResponse},
        session::InMemorySessionStore,
    };
    use llm::usage::Usage;
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;

    /// Waits for an SDK request future to deliver its typed response in ACP tests.
    async fn recv_response<T>(
        request: SentRequest<T>,
    ) -> std::result::Result<T, official_acp::Error>
    where
        T: JsonRpcResponse + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        request.on_receiving_result(async move |result| {
            tx.send(result)
                .map_err(|_| official_acp::Error::internal_error())
        })?;
        rx.await
            .map_err(|_| official_acp::Error::internal_error())?
    }

    #[derive(Clone, Default)]
    struct SharedBufferWriter {
        buffer: Arc<StdMutex<Vec<u8>>>,
    }

    impl SharedBufferWriter {
        /// Returns the captured UTF-8 text written through this shared writer.
        fn rendered(&self) -> String {
            String::from_utf8(
                self.buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            )
            .expect("buffer should be utf8")
        }
    }

    impl io::Write for SharedBufferWriter {
        /// Appends bytes to the shared test buffer.
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        /// Flushes the in-memory writer.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct StubAcpModel {
        responses: Arc<AsyncMutex<VecDeque<ModelResponse>>>,
    }

    impl StubAcpModel {
        /// Builds a stub model that returns queued ACP test responses.
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: Arc::new(AsyncMutex::new(responses.into())),
            }
        }
    }

    #[derive(Clone)]
    struct TimerAcpModel;

    #[async_trait::async_trait(?Send)]
    impl AgentModel for TimerAcpModel {
        /// Uses Tokio time so SDK worker tests verify the per-prompt runtime has drivers enabled.
        async fn complete(&self, _request: ModelRequest) -> KernelResult<ModelResponse> {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            Ok(ModelResponse::text(
                "timer-backed response",
                Usage::default(),
            ))
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AgentModel for StubAcpModel {
        /// Returns queued responses so ACP tests do not call an external provider.
        async fn complete(&self, _request: ModelRequest) -> KernelResult<ModelResponse> {
            self.responses
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| kernel::Error::Runtime {
                    message: "stub ACP model exhausted".to_string(),
                    stage: "acp-test-model-complete".to_string(),
                    inflight_snapshot: None,
                })
        }
    }

    /// Builds the empty tool router used by ACP adapter tests.
    fn empty_router() -> Arc<ToolRouter> {
        Arc::new(ToolRouter::new(
            Arc::new(kernel::tools::ToolRegistry::default()),
            Vec::new(),
        ))
    }

    /// Parses captured ACP output into one JSON value per newline-delimited message.
    fn rendered_messages(writer: &SharedBufferWriter) -> Vec<Value> {
        writer
            .rendered()
            .lines()
            .map(|line| serde_json::from_str(line).expect("ACP line should be JSON"))
            .collect()
    }

    /// Verifies the initialize method advertises the minimum ACP capabilities.
    #[tokio::test]
    async fn initialize_returns_agent_capabilities() {
        let writer = SharedBufferWriter::default();
        let shared = shared_writer(writer.clone());
        let mut agent = AcpAgent::new(
            Arc::new(StubAcpModel::new(Vec::new())),
            Arc::new(InMemorySessionStore::default()),
            empty_router(),
            skills::SkillConfig::default(),
            shared,
            false,
        );

        agent
            .handle_line(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#,
            )
            .await
            .unwrap();

        let messages = rendered_messages(&writer);
        assert_eq!(messages[0]["id"], json!(1));
        assert_eq!(messages[0]["result"]["protocolVersion"], json!(1));
        assert_eq!(
            messages[0]["result"]["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            json!(true)
        );
    }

    /// Verifies adapter errors expose only the JSON-RPC code from `rpc_code`.
    #[test]
    fn error_rpc_code_returns_standard_code_only() {
        let error = Error::MethodNotFound {
            method: "missing/method".to_string(),
            stage: "acp-test".to_string(),
        };

        assert_eq!(error.rpc_code(), -32601);
    }

    /// Verifies adapter errors can still build complete JSON-RPC error objects.
    #[test]
    fn error_to_json_rpc_error_wraps_code_and_message() {
        let error = Error::InvalidRequest {
            message: "bad params".to_string(),
            stage: "acp-test".to_string(),
        };
        let value = error.to_json_rpc_error();

        assert_eq!(value["code"], json!(-32602));
        assert!(value["message"].as_str().is_some());
    }

    /// Verifies session/new creates a unique session id for later prompt turns.
    #[tokio::test]
    async fn session_new_returns_session_id() {
        let writer = SharedBufferWriter::default();
        let shared = shared_writer(writer.clone());
        let mut agent = AcpAgent::new(
            Arc::new(StubAcpModel::new(Vec::new())),
            Arc::new(InMemorySessionStore::default()),
            empty_router(),
            skills::SkillConfig::default(),
            shared,
            false,
        );

        agent
            .handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#)
            .await
            .unwrap();

        let messages = rendered_messages(&writer);
        assert!(messages[0]["result"]["sessionId"].as_str().is_some());
    }

    /// Verifies session/prompt streams user and assistant chunks before the final stop reason.
    #[tokio::test]
    async fn session_prompt_streams_updates_and_final_response() {
        let writer = SharedBufferWriter::default();
        let shared = shared_writer(writer.clone());
        let mut agent = AcpAgent::new(
            Arc::new(StubAcpModel::new(vec![ModelResponse::text(
                "hello from acp",
                Usage::default(),
            )])),
            Arc::new(InMemorySessionStore::default()),
            empty_router(),
            skills::SkillConfig::default(),
            shared,
            false,
        );

        agent
            .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#)
            .await
            .unwrap();
        let session_id = rendered_messages(&writer)[0]["result"]["sessionId"]
            .as_str()
            .expect("session id should be a string")
            .to_string();
        let prompt_request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{
                    "type": "text",
                    "text": "say hello"
                }]
            }
        });

        agent
            .handle_line(&prompt_request.to_string())
            .await
            .unwrap();

        let messages = rendered_messages(&writer);
        assert!(messages.iter().any(|message| {
            message["method"] == json!("session/update")
                && message["params"]["update"]["sessionUpdate"] == json!("user_message_chunk")
        }));
        assert!(messages.iter().any(|message| {
            message["method"] == json!("session/update")
                && message["params"]["update"]["sessionUpdate"] == json!("agent_message_chunk")
                && message["params"]["update"]["content"]["text"] == json!("hello from acp")
        }));
        assert!(messages.iter().any(|message| {
            message["id"] == json!(2) && message["result"]["stopReason"] == json!("end_turn")
        }));
    }

    /// Verifies the official ACP SDK can drive the agent without manual JSON-RPC envelopes.
    #[tokio::test]
    async fn sdk_agent_connection_handles_initialize_session_and_prompt() {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let notifications = Arc::new(AsyncMutex::new(Vec::new()));
        let agent = AcpAgent::new(
            Arc::new(StubAcpModel::new(vec![ModelResponse::text(
                "hello from sdk",
                Usage::default(),
            )])),
            Arc::new(InMemorySessionStore::default()),
            empty_router(),
            skills::SkillConfig::default(),
            shared_writer(io::sink()),
            false,
        );

        tokio::spawn(async move {
            agent
                .connect_sdk(agent_channel)
                .await
                .expect("SDK agent connection should complete");
        });

        let captured_notifications = Arc::clone(&notifications);
        agent_client_protocol::Client
            .builder()
            .on_receive_notification(
                async move |notification: official_acp::SessionNotification, _connection| {
                    captured_notifications.lock().await.push(notification);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(client_channel, async move |connection| {
                let initialize = recv_response(connection.send_request(
                    official_acp::InitializeRequest::new(official_acp::ProtocolVersion::V1),
                ))
                .await?;
                assert_eq!(
                    initialize.protocol_version,
                    official_acp::ProtocolVersion::V1
                );

                let session = recv_response(
                    connection.send_request(official_acp::NewSessionRequest::new("/tmp")),
                )
                .await?;
                let prompt = vec![official_acp::ContentBlock::from("say hello".to_string())];
                let response = recv_response(
                    connection
                        .send_request(official_acp::PromptRequest::new(session.session_id, prompt)),
                )
                .await?;
                assert_eq!(response.stop_reason, official_acp::StopReason::EndTurn);

                Ok(())
            })
            .await
            .expect("SDK client should complete");

        let notifications = notifications.lock().await;
        assert!(notifications.iter().any(|notification| matches!(
            notification.update,
            official_acp::SessionUpdate::UserMessageChunk(_)
        )));
        assert!(notifications.iter().any(|notification| matches!(
            notification.update,
            official_acp::SessionUpdate::AgentMessageChunk(_)
        )));
    }

    /// Verifies SDK prompt workers support Tokio timer/IO drivers used by real providers.
    #[tokio::test]
    async fn sdk_agent_prompt_worker_enables_tokio_drivers() {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let agent = AcpAgent::new(
            Arc::new(TimerAcpModel),
            Arc::new(InMemorySessionStore::default()),
            empty_router(),
            skills::SkillConfig::default(),
            shared_writer(io::sink()),
            false,
        );

        tokio::spawn(async move {
            agent
                .connect_sdk(agent_channel)
                .await
                .expect("SDK agent connection should complete");
        });

        agent_client_protocol::Client
            .builder()
            .connect_with(client_channel, async move |connection| {
                let session = recv_response(
                    connection.send_request(official_acp::NewSessionRequest::new("/tmp")),
                )
                .await?;
                let prompt = vec![official_acp::ContentBlock::from("say hello".to_string())];
                let response = recv_response(
                    connection
                        .send_request(official_acp::PromptRequest::new(session.session_id, prompt)),
                )
                .await?;

                assert_eq!(response.stop_reason, official_acp::StopReason::EndTurn);
                Ok(())
            })
            .await
            .expect("SDK client should complete");
    }

    /// Verifies kernel reasoning deltas are exposed as ACP thought chunks.
    #[tokio::test]
    async fn event_sink_maps_reasoning_to_agent_thought_chunk() {
        let writer = SharedBufferWriter::default();
        let sink = AcpEventSink::new("session-1".to_string(), shared_writer(writer.clone()));

        sink.publish(AgentEvent::ModelReasoningContentDelta {
            id: None,
            text: "thinking".to_string(),
            content_index: 0,
            iteration: Some(1),
        })
        .await;

        let messages = rendered_messages(&writer);
        assert_eq!(messages[0]["method"], json!("session/update"));
        assert_eq!(
            messages[0]["params"]["update"]["sessionUpdate"],
            json!("agent_thought_chunk")
        );
        assert_eq!(
            messages[0]["params"]["update"]["content"]["text"],
            json!("thinking")
        );
        assert!(messages[0]["params"]["update"].get("_meta").is_none());
    }

    /// Verifies completed tool updates omit rawOutput when the kernel did not provide structured output.
    #[tokio::test]
    async fn event_sink_omits_empty_raw_output_for_plain_tool_results() {
        let writer = SharedBufferWriter::default();
        let sink = AcpEventSink::new("session-1".to_string(), shared_writer(writer.clone()));

        sink.publish(AgentEvent::ToolCallCompleted {
            status: ToolCallCompletionStatus::Succeeded,
            name: "example_tool".to_string(),
            handle_id: "call-1".to_string(),
            output: "done".to_string(),
            structured_output: None,
        })
        .await;

        let messages = rendered_messages(&writer);
        assert_eq!(
            messages[0]["params"]["update"]["sessionUpdate"],
            json!("tool_call_update")
        );
        assert!(messages[0]["params"]["update"].get("_meta").is_none());
        assert!(messages[0]["params"]["update"].get("rawOutput").is_none());
    }

    /// Verifies failed tool results become ACP failed updates with the surfaced error message.
    #[tokio::test]
    async fn event_sink_maps_failed_tool_results_to_failed_updates() {
        let writer = SharedBufferWriter::default();
        let sink = AcpEventSink::new("session-1".to_string(), shared_writer(writer.clone()));

        sink.publish(AgentEvent::ToolCallCompleted {
            status: ToolCallCompletionStatus::Failed,
            name: "apply_patch".to_string(),
            handle_id: "call-1".to_string(),
            output: "tool dispatch failed".to_string(),
            structured_output: Some(tools::StructuredToolOutput::failure(
                "apply_patch verification failed: missing Begin/End markers",
            )),
        })
        .await;

        let messages = rendered_messages(&writer);
        assert_eq!(
            messages[0]["params"]["update"]["sessionUpdate"],
            json!("tool_call_update")
        );
        assert_eq!(messages[0]["params"]["update"]["status"], json!("failed"));
        assert_eq!(
            messages[0]["params"]["update"]["content"][0]["content"]["text"],
            json!("tool dispatch failed")
        );
    }

    /// Verifies success/failure ACP mapping follows the explicit event status, not JSON payload shape.
    #[tokio::test]
    async fn event_sink_keeps_successful_json_payloads_completed() {
        let writer = SharedBufferWriter::default();
        let sink = AcpEventSink::new("session-1".to_string(), shared_writer(writer.clone()));

        sink.publish(AgentEvent::ToolCallCompleted {
            status: ToolCallCompletionStatus::Succeeded,
            name: "business_tool".to_string(),
            handle_id: "call-1".to_string(),
            output: "business result".to_string(),
            structured_output: Some(tools::StructuredToolOutput::json_value(json!({
                "success": false,
                "error": {
                    "message": "domain-level false value"
                }
            }))),
        })
        .await;

        let messages = rendered_messages(&writer);
        assert_eq!(
            messages[0]["params"]["update"]["status"],
            json!("completed")
        );
        assert_eq!(
            messages[0]["params"]["update"]["content"][0]["content"]["text"],
            json!("business result")
        );
    }
}
