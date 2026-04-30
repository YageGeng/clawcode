use std::{
    collections::HashMap,
    env,
    fs::File,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::agent::{self, AcpAgent};
use agent_client_protocol::schema as official_acp;
use agent_client_protocol::{Agent as OfficialAgent, Client as OfficialClient, ConnectionTo};
use kernel::{
    SessionTaskContext,
    model::AgentModel,
    tools::{ToolApprovalProfile, router::ToolRouter},
};
use serde_json::Value;
use snafu::{ResultExt, Snafu};
use tools::builtin::{read_text_file::ReadTextFileTool, write_text_file::WriteTextFileTool};

/// Errors produced while serving ACP client-side requests in the human CLI.
#[derive(Debug, Snafu)]
enum ClientHandlerError {
    #[snafu(display("ACP client request is invalid on `{stage}`: {message}"))]
    InvalidParams { message: String, stage: String },

    #[snafu(display("ACP client I/O failed on `{stage}`, {source}"))]
    Io { source: io::Error, stage: String },

    #[snafu(display("ACP client tool failed on `{stage}`, {source}"))]
    Tool { source: tools::Error, stage: String },

    #[snafu(display("ACP client task failed on `{stage}`, {source}"))]
    Join {
        source: tokio::task::JoinError,
        stage: String,
    },
}

type ClientHandlerResult<T> = std::result::Result<T, ClientHandlerError>;

impl From<ClientHandlerError> for official_acp::Error {
    /// Converts client-side handler errors into ACP SDK errors for typed responders.
    fn from(error: ClientHandlerError) -> Self {
        let message = error.to_string();
        match error {
            ClientHandlerError::InvalidParams { .. } => {
                official_acp::Error::invalid_params().data(message)
            }
            ClientHandlerError::Io { .. }
            | ClientHandlerError::Tool { .. }
            | ClientHandlerError::Join { .. } => {
                official_acp::Error::internal_error().data(message)
            }
        }
    }
}

/// Human-facing CLI client backed by a real ACP SDK connection to the agent.
pub struct HumanAcpClient {
    connection: ConnectionTo<OfficialAgent>,
    services: CliClientServices,
    initialized: bool,
    session_id: Option<String>,
    resume_session_id: Option<String>,
}

impl HumanAcpClient {
    /// Builds a human CLI client around an established ACP SDK connection.
    fn new(connection: ConnectionTo<OfficialAgent>, services: CliClientServices) -> Self {
        Self {
            connection,
            services,
            initialized: false,
            session_id: None,
            resume_session_id: None,
        }
    }

    /// Sets the session ID to resume from the persistence store.
    pub fn with_resume_session(mut self, resume_id: String) -> Self {
        self.resume_session_id = Some(resume_id);
        self
    }

    /// Runs one human prompt by sending ACP `initialize`, `session/new`, and `session/prompt`.
    pub async fn run_prompt(
        &mut self,
        prompt: String,
    ) -> std::result::Result<(), official_acp::Error> {
        self.ensure_session().await?;
        let session_id = self.session_id.clone().ok_or_else(|| {
            official_acp::Error::internal_error()
                .data("ACP session was not available after initialization")
        })?;

        self.connection
            .send_request(official_acp::PromptRequest::new(
                session_id,
                vec![official_acp::ContentBlock::from(prompt)],
            ))
            .block_task()
            .await?;
        self.services.finish_turn().map_err(acp_io_error)?;
        Ok(())
    }

    /// Writes the interactive prompt marker through the same output stream used for ACP updates.
    pub fn write_prompt_marker(&self) -> io::Result<()> {
        self.services.write_human_text("> ")
    }

    /// Writes a user-visible error line through the human ACP output stream.
    pub fn write_error_line(&self, error: &dyn std::error::Error) -> io::Result<()> {
        self.services.write_human_line(&format!("error: {error}"))
    }

    /// Creates and caches the ACP session required before prompt turns can run.
    ///
    /// When `resume_session_id` is set, sends `session/load` per the ACP spec
    /// to restore persisted state instead of creating a fresh session.
    async fn ensure_session(&mut self) -> std::result::Result<(), official_acp::Error> {
        if !self.initialized {
            self.connection
                .send_request(
                    official_acp::InitializeRequest::new(official_acp::ProtocolVersion::V1)
                        .client_capabilities(client_capabilities())
                        .client_info(
                            official_acp::Implementation::new(
                                "clawcode-cli",
                                env!("CARGO_PKG_VERSION"),
                            )
                            .title("ClawCode CLI"),
                        ),
                )
                .block_task()
                .await?;
            self.initialized = true;
        }

        if self.session_id.is_none() {
            let cwd = env::current_dir().map_err(acp_io_error)?;
            let root = cwd.canonicalize().map_err(acp_io_error)?;

            if let Some(ref resume_id) = self.resume_session_id {
                // ACP-spec session/load: restore the persisted session by its id.
                self.connection
                    .send_request(official_acp::LoadSessionRequest::new(
                        resume_id.clone(),
                        cwd,
                    ))
                    .block_task()
                    .await?;
                self.services.register_session_root(resume_id, root);
                self.session_id = Some(resume_id.clone());
            } else {
                let response = self
                    .connection
                    .send_request(official_acp::NewSessionRequest::new(cwd))
                    .block_task()
                    .await?;
                let session_id = response.session_id.to_string();
                self.services.register_session_root(&session_id, root);
                self.session_id = Some(session_id);
            }
        }

        Ok(())
    }
}

/// Runtime configuration for an interactive CLI session.
pub struct CliSessionConfig {
    pub skills: skills::SkillConfig,
    pub tool_approval_profile: ToolApprovalProfile,
    pub resume_session_id: Option<String>,
}

/// Runs the interactive human CLI loop while routing every turn through ACP.
///
/// When `resume_session_id` is set in the config, the client requests session
/// resume via `_meta.resumeSessionId` on the `session/new` request.
pub async fn run_interactive_cli_via_acp<M, R, W>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    session_config: CliSessionConfig,
    input: &mut R,
    output: W,
) -> Result<(), Box<dyn std::error::Error>>
where
    M: AgentModel + 'static,
    R: BufRead,
    W: Write + Send + 'static,
{
    let services = CliClientServices::new(output);
    let (client_transport, agent_transport) = memory_transport_pair();
    let mut agent_task = spawn_embedded_agent(
        model,
        store,
        router,
        session_config.skills,
        session_config.tool_approval_profile,
        agent_transport,
    );
    let client_result = run_cli_client(
        services,
        client_transport,
        input,
        session_config.resume_session_id,
    );

    let (result, should_abort_agent) = tokio::select! {
        result = client_result => (result, true),
        agent_result = &mut agent_task => match agent_result {
            Ok(Ok(())) => (Ok(()), false),
            Ok(Err(error)) => (Err(acp_agent_error(error)), false),
            Err(error) => (
                Err(acp_join_to_official_error(
                    error,
                    "acp-client-embedded-agent-task",
                )),
                false,
            ),
        },
    };
    if should_abort_agent {
        agent_task.abort();
        let _ = agent_task.await;
    }
    result.map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
}

/// Builds paired in-memory ACP byte-stream transports for the embedded client and agent.
fn memory_transport_pair() -> (
    impl agent_client_protocol::ConnectTo<OfficialClient>,
    impl agent_client_protocol::ConnectTo<OfficialAgent>,
) {
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let (client_stream, agent_stream) = tokio::io::duplex(1024 * 1024);
    let (client_read, client_write) = tokio::io::split(client_stream);
    let (agent_read, agent_write) = tokio::io::split(agent_stream);
    let client_transport =
        agent_client_protocol::ByteStreams::new(client_write.compat_write(), client_read.compat());
    let agent_transport =
        agent_client_protocol::ByteStreams::new(agent_write.compat_write(), agent_read.compat());
    (client_transport, agent_transport)
}

/// Spawns the embedded ACP agent connected to the in-memory transport.
fn spawn_embedded_agent<M>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    skills: skills::SkillConfig,
    tool_approval_profile: ToolApprovalProfile,
    transport: impl agent_client_protocol::ConnectTo<OfficialAgent> + 'static,
) -> tokio::task::JoinHandle<agent::Result<()>>
where
    M: AgentModel + 'static,
{
    let agent = AcpAgent::new(
        model,
        store,
        router,
        skills,
        agent::shared_writer(std::io::sink()),
        true,
    )
    .with_tool_approval_profile(tool_approval_profile);
    tokio::spawn(async move { agent.connect_sdk(transport).await })
}

/// Runs the ACP SDK client handlers used by the human CLI.
async fn run_cli_client<R>(
    services: CliClientServices,
    transport: impl agent_client_protocol::ConnectTo<OfficialClient> + 'static,
    input: &mut R,
    resume_session_id: Option<String>,
) -> std::result::Result<(), official_acp::Error>
where
    R: BufRead,
{
    let notification_services = services.clone();
    let permission_services = services.clone();
    let read_services = services.clone();
    let write_services = services.clone();
    let prompt_services = services;

    OfficialClient
        .builder()
        .name("clawcode-cli")
        .on_receive_notification(
            async move |notification: official_acp::SessionNotification,
                        _connection: ConnectionTo<OfficialAgent>| {
                notification_services
                    .render_session_update(notification)
                    .map_err(acp_io_error)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: official_acp::RequestPermissionRequest,
                        responder,
                        _connection: ConnectionTo<OfficialAgent>| {
                // Permission prompts perform blocking terminal input, so run them off the SDK
                // dispatch task to keep the ACP connection able to process the response path.
                let services = permission_services.clone();
                let response = tokio::task::spawn_blocking(move || {
                    services.handle_request_permission_response(request)
                })
                .await
                .context(JoinSnafu {
                    stage: "acp-client-permission-blocking-task".to_string(),
                })
                .map_err(official_acp::Error::from)
                .and_then(|response| response);
                match response {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(error),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: official_acp::ReadTextFileRequest,
                        responder,
                        _connection: ConnectionTo<OfficialAgent>| {
                match read_services.handle_fs_read_text_file_response(request) {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(error),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: official_acp::WriteTextFileRequest,
                        responder,
                        _connection: ConnectionTo<OfficialAgent>| {
                match write_services.handle_fs_write_text_file_response(request) {
                    Ok(response) => responder.respond(response),
                    Err(error) => responder.respond_with_error(error),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |connection| {
            let mut client = HumanAcpClient::new(connection, prompt_services);
            if let Some(ref resume_id) = resume_session_id {
                client = client.with_resume_session(resume_id.clone());
            }
            run_prompt_loop(client, input).await
        })
        .await
}

/// Runs the terminal prompt loop over an established ACP client connection.
async fn run_prompt_loop<R>(
    mut client: HumanAcpClient,
    input: &mut R,
) -> std::result::Result<(), official_acp::Error>
where
    R: BufRead,
{
    let mut line = String::new();

    loop {
        client.write_prompt_marker().map_err(acp_io_error)?;

        line.clear();
        if input.read_line(&mut line).map_err(acp_io_error)? == 0 {
            break;
        }

        let prompt = line.trim();
        if prompt.eq_ignore_ascii_case("exit") || prompt.eq_ignore_ascii_case("quit") {
            break;
        }
        if prompt.is_empty() {
            continue;
        }

        if let Err(error) = client.run_prompt(prompt.to_string()).await {
            client.write_error_line(&error).map_err(acp_io_error)?;
        }
    }

    Ok(())
}

/// Shared renderer used by the in-process ACP client for terminal output and local client tools.
#[derive(Clone)]
struct CliClientServices {
    session_roots: Arc<Mutex<SessionRoots>>,
    renderer: Arc<Mutex<CliRenderer>>,
    approval_input: Arc<dyn ApprovalInput>,
}

impl CliClientServices {
    /// Builds shared CLI client services for rendering, filesystem capabilities, and approval.
    fn new<W>(output: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self {
            session_roots: Arc::new(Mutex::new(SessionRoots::default())),
            renderer: Arc::new(Mutex::new(CliRenderer::new(output))),
            approval_input: Arc::new(TtyApprovalInput),
        }
    }

    /// Registers the filesystem root exposed for an ACP session.
    fn register_session_root(&self, session_id: &str, root: PathBuf) {
        let mut roots = self
            .session_roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        roots.register(session_id, root);
    }

    /// Writes a raw human-facing text fragment to the underlying output stream.
    fn write_human_text(&self, text: &str) -> io::Result<()> {
        let mut renderer = self
            .renderer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        renderer.write_human_text(text)
    }

    /// Writes a raw human-facing line to the underlying output stream.
    fn write_human_line(&self, text: &str) -> io::Result<()> {
        let mut renderer = self
            .renderer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        renderer.write_status_line(text)
    }

    /// Closes any open streamed line after a prompt turn finishes.
    fn finish_turn(&self) -> io::Result<()> {
        let mut renderer = self
            .renderer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        renderer.finish_text_line_if_needed()
    }

    /// Renders an ACP session notification received through the SDK client connection.
    fn render_session_update(
        &self,
        notification: official_acp::SessionNotification,
    ) -> io::Result<()> {
        let mut renderer = self
            .renderer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        renderer.render_session_update(notification)
    }

    /// Handles an ACP permission request and returns the typed SDK response.
    fn handle_request_permission_response(
        &self,
        request: official_acp::RequestPermissionRequest,
    ) -> std::result::Result<official_acp::RequestPermissionResponse, official_acp::Error> {
        self.handle_request_permission_typed(request)
            .map_err(official_acp::Error::from)
    }

    /// Handles an ACP read request and returns the typed SDK response.
    fn handle_fs_read_text_file_response(
        &self,
        request: official_acp::ReadTextFileRequest,
    ) -> std::result::Result<official_acp::ReadTextFileResponse, official_acp::Error> {
        self.handle_fs_read_text_file_typed(request)
            .map_err(official_acp::Error::from)
    }

    /// Handles an ACP write request and returns the typed SDK response.
    fn handle_fs_write_text_file_response(
        &self,
        request: official_acp::WriteTextFileRequest,
    ) -> std::result::Result<official_acp::WriteTextFileResponse, official_acp::Error> {
        self.handle_fs_write_text_file_typed(request)
            .map_err(official_acp::Error::from)
    }
}

/// Session root registry used by ACP client filesystem capabilities.
#[derive(Default)]
struct SessionRoots {
    roots: HashMap<String, PathBuf>,
}

impl SessionRoots {
    /// Registers the canonical filesystem root exposed for one ACP session.
    fn register(&mut self, session_id: &str, root: PathBuf) {
        self.roots
            .insert(session_id.to_string(), canonicalize_best_effort(root));
    }

    /// Returns the registered root for one session or a typed invalid-params error.
    fn get(&self, session_id: &str, stage: &str) -> ClientHandlerResult<&PathBuf> {
        self.roots
            .get(session_id)
            .ok_or_else(|| ClientHandlerError::InvalidParams {
                message: format!("unknown sessionId `{session_id}`"),
                stage: stage.to_string(),
            })
    }
}

/// Terminal renderer for the human ACP client.
struct CliRenderer {
    presentation: CliPresentationState,
    output: Box<dyn Write + Send>,
}

impl CliRenderer {
    /// Builds a terminal renderer around the provided output stream.
    fn new<W>(output: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self {
            presentation: CliPresentationState::default(),
            output: Box::new(output),
        }
    }

    /// Writes a raw human-facing text fragment to the terminal output stream.
    fn write_human_text(&mut self, text: &str) -> io::Result<()> {
        self.output.write_all(text.as_bytes())?;
        self.output.flush()
    }
}

/// Input source used for interactive approval prompts.
trait ApprovalInput: Send + Sync {
    /// Reads one approval answer line.
    fn read_line(&self, answer: &mut String) -> io::Result<usize>;
}

/// Approval input backed by the controlling terminal.
struct TtyApprovalInput;

impl ApprovalInput for TtyApprovalInput {
    /// Reads from the controlling terminal instead of the prompt loop's stdin lock.
    fn read_line(&self, answer: &mut String) -> io::Result<usize> {
        #[cfg(unix)]
        {
            let tty = File::open("/dev/tty")?;
            let mut reader = io::BufReader::new(tty);
            reader.read_line(answer)
        }

        #[cfg(windows)]
        {
            let tty = File::open("CONIN$")?;
            let mut reader = io::BufReader::new(tty);
            return reader.read_line(answer);
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = answer;
            Err(io::Error::other(
                "approval prompt input is not supported on this platform",
            ))
        }
    }
}

impl CliClientServices {
    /// Handles an ACP fs/read_text_file request and returns the typed SDK response.
    fn handle_fs_read_text_file_typed(
        &self,
        request: official_acp::ReadTextFileRequest,
    ) -> ClientHandlerResult<official_acp::ReadTextFileResponse> {
        let session_id = request.session_id.to_string();
        let root = {
            let roots = self
                .session_roots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            roots
                .get(&session_id, "acp-client-fs-read-session")?
                .clone()
        };
        let output = ReadTextFileTool::new(root)
            .read_text_file(request.path, request.line, request.limit)
            .context(ToolSnafu {
                stage: "acp-client-fs-read".to_string(),
            })?;
        let content = output.text;

        Ok(official_acp::ReadTextFileResponse::new(content))
    }

    /// Handles an ACP fs/write_text_file request and returns the typed SDK response.
    fn handle_fs_write_text_file_typed(
        &self,
        request: official_acp::WriteTextFileRequest,
    ) -> ClientHandlerResult<official_acp::WriteTextFileResponse> {
        let session_id = request.session_id.to_string();
        let root = {
            let roots = self
                .session_roots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            roots
                .get(&session_id, "acp-client-fs-write-session")?
                .clone()
        };
        WriteTextFileTool::new(root)
            .write_text_file(request.path, request.content)
            .context(ToolSnafu {
                stage: "acp-client-fs-write".to_string(),
            })?;

        Ok(official_acp::WriteTextFileResponse::new())
    }

    /// Handles an ACP session/request_permission request and returns the typed SDK response.
    fn handle_request_permission_typed(
        &self,
        request: official_acp::RequestPermissionRequest,
    ) -> ClientHandlerResult<official_acp::RequestPermissionResponse> {
        let tool_name = request
            .tool_call
            .fields
            .title
            .as_deref()
            .unwrap_or("tool call");
        let raw_input = request
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_else(|| "{}".to_string());
        self.renderer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .write_status_line(&format!(
                "{tool_name} for session `{}` with args {raw_input}",
                request.session_id
            ))
            .context(IoSnafu {
                stage: "acp-client-permission-render-request".to_string(),
            })?;

        let allow_option = request.options.iter().find(|option| {
            matches!(
                option.kind,
                official_acp::PermissionOptionKind::AllowOnce
                    | official_acp::PermissionOptionKind::AllowAlways
            )
        });
        let reject_option = request.options.iter().find(|option| {
            matches!(
                option.kind,
                official_acp::PermissionOptionKind::RejectOnce
                    | official_acp::PermissionOptionKind::RejectAlways
            )
        });

        self.renderer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .write_status_line("Select permission: [y] allow once, [n] reject once")
            .context(IoSnafu {
                stage: "acp-client-permission-render-options".to_string(),
            })?;
        let mut answer = String::new();
        self.approval_input
            .read_line(&mut answer)
            .context(IoSnafu {
                stage: "acp-client-permission-read-answer".to_string(),
            })?;

        let selected = if matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            allow_option.ok_or_else(|| ClientHandlerError::InvalidParams {
                message: "permission request has no allow option".to_string(),
                stage: "acp-client-permission-allow-option".to_string(),
            })?
        } else {
            reject_option.ok_or_else(|| ClientHandlerError::InvalidParams {
                message: "permission request has no reject option".to_string(),
                stage: "acp-client-permission-reject-option".to_string(),
            })?
        };

        Ok(official_acp::RequestPermissionResponse::new(
            official_acp::RequestPermissionOutcome::Selected(
                official_acp::SelectedPermissionOutcome::new(selected.option_id.clone()),
            ),
        ))
    }
}

impl CliRenderer {
    /// Renders one ACP `session/update` notification into the existing CLI presentation style.
    fn render_session_update(
        &mut self,
        notification: official_acp::SessionNotification,
    ) -> io::Result<()> {
        match notification.update {
            official_acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                self.render_text_chunk(&chunk, TextRenderKind::Think)?;
            }
            official_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                self.render_text_chunk(&chunk, TextRenderKind::Answer)?;
            }
            official_acp::SessionUpdate::ToolCall(tool_call) => {
                let arguments = tool_call
                    .raw_input
                    .as_ref()
                    .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_else(|| "{}".to_string());
                self.write_status_line(&format!(
                    "[tool] {} started args={arguments}",
                    tool_call.title
                ))?;
            }
            official_acp::SessionUpdate::ToolCallUpdate(tool_call_update) => {
                let title = tool_call_update.fields.title.as_deref().unwrap_or("tool");
                match tool_call_update.fields.status {
                    Some(official_acp::ToolCallStatus::Completed) => {
                        self.write_status_line(&format!("[tool] {title} completed"))?;
                    }
                    Some(official_acp::ToolCallStatus::Failed) => {
                        if let Some(error_text) =
                            tool_call_update
                                .fields
                                .content
                                .as_ref()
                                .and_then(|content| {
                                    content.iter().find_map(|content| match content {
                                        official_acp::ToolCallContent::Content(content) => {
                                            content_block_text(&content.content)
                                        }
                                        _ => None,
                                    })
                                })
                        {
                            self.write_status_line(&format!(
                                "[tool] {title} failed: {error_text}"
                            ))?;
                        } else {
                            self.write_status_line(&format!("[tool] {title} failed"))?;
                        }
                    }
                    _ => {}
                }
            }
            official_acp::SessionUpdate::UserMessageChunk(_) => {}
            _ => {}
        }
        Ok(())
    }

    /// Renders a text chunk according to the ACP session update variant that carried it.
    fn render_text_chunk(
        &mut self,
        chunk: &official_acp::ContentChunk,
        kind: TextRenderKind,
    ) -> io::Result<()> {
        let Some(text) = content_chunk_text(chunk) else {
            return Ok(());
        };

        match kind {
            TextRenderKind::Think => self.write_reasoning_delta(text),
            TextRenderKind::Answer => self.write_text_delta(text),
        }
    }

    /// Writes streamed assistant text directly to the terminal and keeps the line open.
    fn write_text_delta(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        self.begin_text_section(TextRenderKind::Answer)?;
        if self.presentation.reasoning_line_open {
            writeln!(self.output)?;
            self.presentation.reasoning_line_open = false;
        }
        write!(self.output, "{text}")?;
        self.output.flush()?;
        self.presentation.text_line_open = true;
        Ok(())
    }

    /// Writes streamed reasoning content before visible answer text.
    fn write_reasoning_delta(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        self.begin_text_section(TextRenderKind::Think)?;
        if self.presentation.text_line_open {
            writeln!(self.output)?;
            self.presentation.text_line_open = false;
        }
        write!(self.output, "{text}")?;
        self.output.flush()?;
        self.presentation.reasoning_line_open = true;
        Ok(())
    }

    /// Starts a visible CLI text section when the streamed update kind changes.
    fn begin_text_section(&mut self, kind: TextRenderKind) -> io::Result<()> {
        if self.presentation.active_text_kind == Some(kind) {
            return Ok(());
        }

        if self.presentation.text_line_open || self.presentation.reasoning_line_open {
            writeln!(self.output)?;
        }
        writeln!(self.output, "{}", kind.cli_label())?;
        self.presentation.text_line_open = false;
        self.presentation.reasoning_line_open = false;
        self.presentation.active_text_kind = Some(kind);
        Ok(())
    }

    /// Writes a standalone status line after closing any open streamed text line.
    fn write_status_line(&mut self, line: &str) -> io::Result<()> {
        if self.presentation.text_line_open || self.presentation.reasoning_line_open {
            writeln!(self.output)?;
        }
        writeln!(self.output, "{line}")?;
        self.output.flush()?;
        self.presentation.text_line_open = false;
        self.presentation.reasoning_line_open = false;
        self.presentation.active_text_kind = None;
        Ok(())
    }

    /// Closes the current streamed text line so the next prompt starts on a clean line.
    fn finish_text_line_if_needed(&mut self) -> io::Result<()> {
        if self.presentation.text_line_open || self.presentation.reasoning_line_open {
            writeln!(self.output)?;
            self.output.flush()?;
            self.presentation.text_line_open = false;
            self.presentation.reasoning_line_open = false;
        }
        self.presentation.active_text_kind = None;
        Ok(())
    }
}

/// Text render channel selected from ACP session update variants.
#[derive(Clone, Copy, Eq, PartialEq)]
enum TextRenderKind {
    Think,
    Answer,
}

impl TextRenderKind {
    /// Returns the human-facing CLI section label for this text stream kind.
    fn cli_label(self) -> &'static str {
        match self {
            Self::Think => "[think]",
            Self::Answer => "[answer]",
        }
    }
}

/// Builds the typed client capability block advertised during ACP initialization.
fn client_capabilities() -> official_acp::ClientCapabilities {
    official_acp::ClientCapabilities::new()
        .fs(official_acp::FileSystemCapabilities::new()
            .read_text_file(true)
            .write_text_file(true))
        .terminal(false)
}

/// Returns a canonical path when available, otherwise preserves the caller-provided path.
fn canonicalize_best_effort(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Returns the text carried by an ACP content chunk when it is text content.
fn content_chunk_text(chunk: &official_acp::ContentChunk) -> Option<&str> {
    content_block_text(&chunk.content)
}

/// Returns the text carried by an ACP content block when it is text content.
fn content_block_text(block: &official_acp::ContentBlock) -> Option<&str> {
    match block {
        official_acp::ContentBlock::Text(text) => Some(text.text.as_str()),
        _ => None,
    }
}

/// Converts an I/O error into the ACP SDK error type used by client callbacks.
fn acp_io_error(error: io::Error) -> official_acp::Error {
    official_acp::Error::internal_error().data(error.to_string())
}

/// Converts an embedded ACP agent error into the ACP SDK error type.
fn acp_agent_error(error: agent::Error) -> official_acp::Error {
    official_acp::Error::internal_error().data(error.to_string())
}

/// Converts a task join failure into the ACP SDK error type.
fn acp_join_to_official_error(error: tokio::task::JoinError, stage: &str) -> official_acp::Error {
    official_acp::Error::internal_error().data(format!("{stage}: {error}"))
}

/// Tracks whether the human CLI renderer currently has an open streamed line.
#[derive(Default)]
struct CliPresentationState {
    text_line_open: bool,
    reasoning_line_open: bool,
    active_text_kind: Option<TextRenderKind>,
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use agent_client_protocol::schema as official_acp;
    use serde_json::json;
    use tools::{ToolCallRequest, ToolContext};

    use crate::message::AcpMessage;

    use super::CliClientServices;

    #[derive(Clone, Default)]
    struct SharedBufferWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBufferWriter {
        /// Returns captured terminal output as UTF-8 text.
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

    impl Write for SharedBufferWriter {
        /// Appends bytes to the shared buffer.
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        /// Flushes the in-memory output.
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Verifies the local ACP client advertises filesystem RPC support.
    #[test]
    fn client_capabilities_advertise_filesystem_methods() {
        let capabilities =
            serde_json::to_value(super::client_capabilities()).expect("capabilities serialize");

        assert_eq!(capabilities["fs"]["readTextFile"], json!(true));
        assert_eq!(capabilities["fs"]["writeTextFile"], json!(true));
    }

    /// Verifies ACP fs/read_text_file reads from a registered session root.
    #[test]
    fn client_handles_fs_read_text_file_requests() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let file_path = workspace.path().join("src.txt");
        std::fs::write(&file_path, "one\ntwo\nthree\n").expect("file should be written");
        let writer = CliClientServices::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        let response = writer
            .handle_fs_read_text_file_response(
                official_acp::ReadTextFileRequest::new("session-1", file_path)
                    .line(2)
                    .limit(1),
            )
            .expect("fs response should be returned");

        assert_eq!(response.content, "two\n");
    }

    /// Verifies ACP fs/read_text_file resolves relative paths from the registered session root.
    #[test]
    fn client_handles_relative_fs_read_text_file_requests() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        std::fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n")
            .expect("file should be written");
        let writer = CliClientServices::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        let response = writer
            .handle_fs_read_text_file_response(official_acp::ReadTextFileRequest::new(
                "session-1",
                "Cargo.toml",
            ))
            .expect("fs response should be returned");

        assert_eq!(response.content, "[workspace]\n");
    }

    /// Rejects ACP fs/read_text_file traversal paths outside the session root.
    #[test]
    fn client_rejects_relative_read_text_file_path_traversal() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let parent = workspace
            .path()
            .parent()
            .expect("workspace should have a parent");
        let outside = parent.join("outside_read.txt");
        std::fs::write(&outside, "outside-file").expect("outside file should be written");

        let writer = CliClientServices::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        let error = writer
            .handle_fs_read_text_file_response(official_acp::ReadTextFileRequest::new(
                "session-1",
                "../outside_read.txt",
            ))
            .expect_err("path traversal should be rejected");

        assert!(
            error
                .to_string()
                .contains("path must stay inside the tool root")
        );
        assert_eq!(
            std::fs::read_to_string(outside).expect("outside file should still be readable"),
            "outside-file"
        );
    }

    /// Verifies ACP fs/write_text_file writes inside a registered session root.
    #[test]
    fn client_handles_fs_write_text_file_requests() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let file_path = workspace.path().join("created.txt");
        let writer = CliClientServices::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writer
            .handle_fs_write_text_file_response(official_acp::WriteTextFileRequest::new(
                "session-1",
                file_path.clone(),
                "created through ACP",
            ))
            .expect("fs response should be returned");

        assert_eq!(
            std::fs::read_to_string(file_path).expect("file should be readable"),
            "created through ACP"
        );
    }

    /// Rejects ACP fs/write_text_file traversal paths outside the session root.
    #[test]
    fn client_rejects_relative_write_text_file_path_traversal() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let parent = workspace
            .path()
            .parent()
            .expect("workspace should have a parent");
        let outside = parent.join("outside_write.txt");
        std::fs::write(&outside, "outside-file").expect("outside file should be written");

        let writer = CliClientServices::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writer
            .handle_fs_write_text_file_response(official_acp::WriteTextFileRequest::new(
                "session-1",
                "../outside_write.txt",
                "should not write",
            ))
            .expect_err("path traversal should be rejected");

        assert_eq!(
            std::fs::read_to_string(outside).expect("outside file should still be readable"),
            "outside-file"
        );
    }

    /// Verifies CLI output labels reasoning and answer sections clearly.
    #[test]
    fn client_renders_reasoning_and_answer_with_section_labels() {
        let output = SharedBufferWriter::default();
        let writer = CliClientServices::new(output.clone());

        writer
            .render_session_update(official_acp::SessionNotification::new(
                "session-1",
                AcpMessage::thought_text("thinking"),
            ))
            .expect("reasoning update should render");
        writer
            .render_session_update(official_acp::SessionNotification::new(
                "session-1",
                AcpMessage::agent_text("answer"),
            ))
            .expect("answer update should render");

        assert_eq!(output.rendered(), "[think]\nthinking\n[answer]\nanswer");
    }

    /// Verifies failed tool updates are rendered with failed status and the error text.
    #[test]
    fn client_renders_failed_tool_updates_with_error_details() {
        let output = SharedBufferWriter::default();
        let writer = CliClientServices::new(output.clone());

        writer
            .render_session_update(official_acp::SessionNotification::new(
                "session-1",
                AcpMessage::tool_failed(
                    "call-1",
                    "apply_patch",
                    "apply_patch verification failed: missing Begin/End markers",
                    None,
                ),
            ))
            .expect("failed tool update should render");

        assert_eq!(
            output.rendered(),
            "[tool] apply_patch failed: apply_patch verification failed: missing Begin/End markers\n"
        );
    }

    /// Verifies filesystem tools are visible through the default tool router.
    #[tokio::test]
    async fn router_exposes_acp_filesystem_tools() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let file_path = workspace.path().join("tool-created.txt");
        let router = tools::ToolRouter::from_path(workspace.path()).await;

        let definitions = router.definitions();
        let definition_names = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        assert!(definition_names.contains(&"fs/read_text_file"));
        assert!(definition_names.contains(&"fs/write_text_file"));

        router
            .dispatch(
                ToolCallRequest {
                    id: "call-1".to_string(),
                    call_id: None,
                    name: "fs/write_text_file".to_string(),
                    arguments: json!({
                        "path": file_path,
                        "content": "written through tool"
                    }),
                },
                ToolContext::new("session-1", "thread-1"),
            )
            .await
            .expect("fs write tool should dispatch");
        assert_eq!(
            std::fs::read_to_string(file_path).expect("file should be readable"),
            "written through tool"
        );
    }
}
