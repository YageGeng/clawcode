use std::{
    collections::HashMap,
    env,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use agent_client_protocol::schema as official_acp;
use agent_client_protocol::{JsonRpcMessage, JsonRpcRequest};
use kernel::{SessionTaskContext, model::AgentModel, tools::router::ToolRouter};
use serde_json::{Value, json};
use tools::builtin::{read_text_file::ReadTextFileTool, write_text_file::WriteTextFileTool};
use tracing::{debug, info};

use crate::{
    agent::{self, AcpAgent},
    message::SessionRpc,
};

/// Human-facing CLI client that talks to the in-process agent exclusively through ACP messages.
pub struct HumanAcpClient<M> {
    agent: AcpAgent<M>,
    writer: AcpCliWriter,
    next_id: u64,
    initialized: bool,
    session_id: Option<String>,
}

impl<M> HumanAcpClient<M>
where
    M: AgentModel + 'static,
{
    /// Builds a human CLI ACP client around an existing model, store, router, and output stream.
    pub async fn new<W>(
        model: Arc<M>,
        store: Arc<SessionTaskContext>,
        router: Arc<ToolRouter>,
        skills: skills::SkillConfig,
        output: W,
    ) -> Self
    where
        W: Write + Send + 'static,
    {
        let writer = AcpCliWriter::new(output);
        let agent = AcpAgent::new(
            model,
            store,
            router,
            skills,
            agent::shared_writer(writer.clone()),
            true,
        );

        Self {
            agent,
            writer,
            next_id: 1,
            initialized: false,
            session_id: None,
        }
    }

    /// Runs one human prompt by sending ACP `initialize`, `session/new`, and `session/prompt`.
    pub async fn run_prompt(&mut self, prompt: String) -> Result<(), Box<dyn std::error::Error>> {
        self.ensure_session().await?;
        let session_id = self.session_id.clone().ok_or_else(|| {
            io::Error::other("ACP session was not available after initialization")
        })?;

        self.send_request(official_acp::PromptRequest::new(
            session_id,
            vec![official_acp::ContentBlock::from(prompt)],
        ))
        .await?;
        self.writer.finish_turn()?;
        Ok(())
    }

    /// Writes the interactive prompt marker through the same output stream used for ACP updates.
    pub fn write_prompt_marker(&self) -> io::Result<()> {
        self.writer.write_human_text("> ")
    }

    /// Writes a user-visible error line through the human ACP output stream.
    pub fn write_error_line(&self, error: &dyn std::error::Error) -> io::Result<()> {
        self.writer.write_human_line(&format!("error: {error}"))
    }

    /// Creates and caches the ACP session required before prompt turns can run.
    async fn ensure_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.initialized {
            self.send_request(
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
            .await?;
            self.initialized = true;
        }

        if self.session_id.is_none() {
            let cwd = env::current_dir()?;
            let result = self
                .send_request(official_acp::NewSessionRequest::new(cwd.clone()))
                .await?;
            let response: official_acp::NewSessionResponse =
                serde_json::from_value(result).map_err(io::Error::other)?;
            let session_id = response.session_id.to_string();
            self.writer
                .register_session_root(&session_id, cwd.canonicalize()?);
            self.session_id = Some(session_id);
        }

        Ok(())
    }

    /// Sends one typed JSON-RPC request to the ACP agent and returns the dynamic result object.
    async fn send_request<R>(&mut self, request: R) -> Result<Value, Box<dyn std::error::Error>>
    where
        R: JsonRpcRequest,
    {
        let id = self.next_id;
        self.next_id += 1;
        let request = SessionRpc::request(id, &request);

        info!(direction = "send", payload = %request, "acp protocol message");
        self.agent.handle_line(&request.to_string()).await?;
        let response = self
            .writer
            .take_response(id)
            .ok_or_else(|| io::Error::other(format!("ACP response `{id}` was not received")))?;
        info!(direction = "recv", payload = %response, "acp protocol message");

        if let Some(error) = response.get("error") {
            return Err(io::Error::other(error.to_string()).into());
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Runs the interactive human CLI loop while routing every turn through ACP.
pub async fn run_interactive_cli_via_acp<M, R, W>(
    model: Arc<M>,
    store: Arc<SessionTaskContext>,
    router: Arc<ToolRouter>,
    skills: skills::SkillConfig,
    input: &mut R,
    output: W,
) -> Result<(), Box<dyn std::error::Error>>
where
    M: AgentModel + 'static,
    R: BufRead,
    W: Write + Send + 'static,
{
    let mut client = HumanAcpClient::new(model, store, router, skills, output).await;
    let mut line = String::new();

    loop {
        client.write_prompt_marker()?;

        line.clear();
        if input.read_line(&mut line)? == 0 {
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
            client.write_error_line(error.as_ref())?;
        }
    }

    Ok(())
}

/// Shared writer used by the in-process ACP client to render notifications and retain responses.
#[derive(Clone)]
struct AcpCliWriter {
    state: Arc<Mutex<AcpCliWriterState>>,
}

impl AcpCliWriter {
    /// Builds a writer that renders ACP session updates into human-readable CLI output.
    fn new<W>(output: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self {
            state: Arc::new(Mutex::new(AcpCliWriterState {
                pending_line: Vec::new(),
                responses: Vec::new(),
                session_roots: HashMap::new(),
                presentation: CliPresentationState::default(),
                output: Box::new(output),
            })),
        }
    }

    /// Registers the filesystem root exposed for an ACP session.
    fn register_session_root(&self, session_id: &str, root: PathBuf) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .session_roots
            .insert(session_id.to_string(), canonicalize_best_effort(root));
    }

    /// Removes and returns a JSON-RPC response for the requested id.
    fn take_response(&self, id: u64) -> Option<Value> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = state
            .responses
            .iter()
            .position(|response| response.get("id") == Some(&json!(id)))?;
        Some(state.responses.remove(index))
    }

    /// Writes a raw human-facing text fragment to the underlying output stream.
    fn write_human_text(&self, text: &str) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.output.write_all(text.as_bytes())?;
        state.output.flush()
    }

    /// Writes a raw human-facing line to the underlying output stream.
    fn write_human_line(&self, text: &str) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.write_status_line(text)
    }

    /// Closes any open streamed line after a prompt turn finishes.
    fn finish_turn(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.finish_text_line_if_needed()
    }
}

impl Write for AcpCliWriter {
    /// Accepts newline-delimited ACP JSON-RPC messages from the in-process agent.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for byte in buf {
            if *byte == b'\n' {
                let line = String::from_utf8(std::mem::take(&mut state.pending_line))
                    .map_err(io::Error::other)?;
                state.process_acp_line(&line)?;
            } else {
                state.pending_line.push(*byte);
            }
        }

        Ok(buf.len())
    }

    /// Flushes the wrapped human output stream.
    fn flush(&mut self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.output.flush()
    }
}

/// Mutable state shared by ACP writer clones.
struct AcpCliWriterState {
    pending_line: Vec<u8>,
    responses: Vec<Value>,
    session_roots: HashMap<String, PathBuf>,
    presentation: CliPresentationState,
    output: Box<dyn Write + Send>,
}

impl AcpCliWriterState {
    /// Parses one ACP JSON-RPC line and either renders a notification or stores a response.
    fn process_acp_line(&mut self, line: &str) -> io::Result<()> {
        info!(direction = "recv", payload = %line, "acp protocol message");
        let message: Value = serde_json::from_str(line).map_err(io::Error::other)?;
        if let Some(notification) = agent_notification(&message)? {
            self.render_session_update(notification)?;
        } else if message.get("method").is_some() {
            self.handle_client_request(&message);
        } else if message.get("id").is_some() {
            debug!(id = ?message.get("id"), "acp response buffered");
            self.responses.push(message);
        }
        Ok(())
    }

    /// Handles one agent-initiated ACP client request and buffers the JSON-RPC response.
    fn handle_client_request(&mut self, message: &Value) {
        let Some(id) = message.get("id").cloned() else {
            return;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match official_acp::AgentRequest::parse_message(method, &params) {
            Ok(official_acp::AgentRequest::ReadTextFileRequest(request)) => {
                self.handle_fs_read_text_file(request)
            }
            Ok(official_acp::AgentRequest::WriteTextFileRequest(request)) => {
                self.handle_fs_write_text_file(request)
            }
            Ok(_) => Err(json_rpc_client_error(
                -32601,
                format!("ACP client method `{method}` is not supported"),
            )),
            _ => Err(json_rpc_client_error(
                -32601,
                format!("ACP client method `{method}` is not supported"),
            )),
        };
        let response = match result {
            Ok(result) => SessionRpc::response(id, result),
            Err(error) => SessionRpc::error(id, error),
        };
        info!(direction = "send", payload = %response, "acp protocol message");
        self.responses.push(response);
    }

    /// Handles an ACP fs/read_text_file request from the agent.
    fn handle_fs_read_text_file(
        &self,
        request: official_acp::ReadTextFileRequest,
    ) -> Result<Value, Value> {
        let session_id = request.session_id.to_string();
        let root = self.session_roots.get(&session_id).ok_or_else(|| {
            json_rpc_client_error(-32602, format!("unknown sessionId `{session_id}`"))
        })?;
        let output = ReadTextFileTool::new(root.clone())
            .read_text_file(request.path, request.line, request.limit)
            .map_err(|error| json_rpc_client_error(-32603, error.to_string()))?;
        let content = output.text;

        Ok(SessionRpc::value(&official_acp::ReadTextFileResponse::new(
            content,
        )))
    }

    /// Handles an ACP fs/write_text_file request from the agent.
    fn handle_fs_write_text_file(
        &self,
        request: official_acp::WriteTextFileRequest,
    ) -> Result<Value, Value> {
        let session_id = request.session_id.to_string();
        let root = self.session_roots.get(&session_id).ok_or_else(|| {
            json_rpc_client_error(-32602, format!("unknown sessionId `{session_id}`"))
        })?;
        WriteTextFileTool::new(root.clone())
            .write_text_file(request.path, request.content)
            .map_err(|error| json_rpc_client_error(-32603, error.to_string()))?;

        Ok(SessionRpc::value(
            &official_acp::WriteTextFileResponse::new(),
        ))
    }

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
                if matches!(
                    tool_call_update.fields.status,
                    Some(official_acp::ToolCallStatus::Completed)
                ) {
                    let title = tool_call_update.fields.title.as_deref().unwrap_or("tool");
                    self.write_status_line(&format!("[tool] {title} completed"))?;
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

/// Parses an agent notification from a raw JSON-RPC message when the method is a notification.
fn agent_notification(message: &Value) -> io::Result<Option<official_acp::SessionNotification>> {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match official_acp::AgentNotification::parse_message(method, &params) {
        Ok(official_acp::AgentNotification::SessionNotification(notification)) => {
            Ok(Some(notification))
        }
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
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

/// Builds a JSON-RPC client error object for ACP client-side method handling.
fn json_rpc_client_error(code: i64, message: impl ToString) -> Value {
    json!({
        "code": code,
        "message": message.to_string(),
    })
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

    use serde_json::json;
    use tools::{ToolCallRequest, ToolContext};

    use crate::message::AcpMessage;

    use super::{AcpCliWriter, SessionRpc};

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
        let capabilities = SessionRpc::value(&super::client_capabilities());

        assert_eq!(capabilities["fs"]["readTextFile"], json!(true));
        assert_eq!(capabilities["fs"]["writeTextFile"], json!(true));
    }

    /// Verifies ACP fs/read_text_file reads from a registered session root.
    #[test]
    fn client_handles_fs_read_text_file_requests() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let file_path = workspace.path().join("src.txt");
        std::fs::write(&file_path, "one\ntwo\nthree\n").expect("file should be written");
        let mut writer = AcpCliWriter::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writeln!(
            writer,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "fs/read_text_file",
                "params": {
                    "sessionId": "session-1",
                    "path": file_path,
                    "line": 2,
                    "limit": 1
                }
            })
        )
        .expect("request should be written");

        let response = writer
            .take_response(10)
            .expect("fs response should be buffered");
        assert_eq!(response["result"]["content"], json!("two\n"));
    }

    /// Verifies ACP fs/read_text_file resolves relative paths from the registered session root.
    #[test]
    fn client_handles_relative_fs_read_text_file_requests() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        std::fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n")
            .expect("file should be written");
        let mut writer = AcpCliWriter::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writeln!(
            writer,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 12,
                "method": "fs/read_text_file",
                "params": {
                    "sessionId": "session-1",
                    "path": "Cargo.toml"
                }
            })
        )
        .expect("request should be written");

        let response = writer
            .take_response(12)
            .expect("fs response should be buffered");
        assert_eq!(response["result"]["content"], json!("[workspace]\n"));
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

        let mut writer = AcpCliWriter::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writeln!(
            writer,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 13,
                "method": "fs/read_text_file",
                "params": {
                    "sessionId": "session-1",
                    "path": "../outside_read.txt"
                }
            })
        )
        .expect("request should be written");

        let response = writer
            .take_response(13)
            .expect("fs response should be buffered");
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(
            response["error"]["message"],
            json!("filesystem path must stay inside the session root")
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
        let mut writer = AcpCliWriter::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writeln!(
            writer,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "fs/write_text_file",
                "params": {
                    "sessionId": "session-1",
                    "path": file_path,
                    "content": "created through ACP"
                }
            })
        )
        .expect("request should be written");

        let response = writer
            .take_response(11)
            .expect("fs response should be buffered");
        assert_eq!(response["result"], json!({}));
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

        let mut writer = AcpCliWriter::new(Vec::<u8>::new());
        writer.register_session_root("session-1", workspace.path().to_path_buf());

        writeln!(
            writer,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 14,
                "method": "fs/write_text_file",
                "params": {
                    "sessionId": "session-1",
                    "path": "../outside_write.txt",
                    "content": "should not write"
                }
            })
        )
        .expect("request should be written");

        let response = writer
            .take_response(14)
            .expect("fs response should be buffered");
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(
            std::fs::read_to_string(outside).expect("outside file should still be readable"),
            "outside-file"
        );
    }

    /// Verifies CLI output labels reasoning and answer sections clearly.
    #[test]
    fn client_renders_reasoning_and_answer_with_section_labels() {
        let output = SharedBufferWriter::default();
        let mut writer = AcpCliWriter::new(output.clone());

        writeln!(
            writer,
            "{}",
            SessionRpc::session_update("session-1", AcpMessage::thought_text("thinking"))
        )
        .expect("reasoning update should be written");
        writeln!(
            writer,
            "{}",
            SessionRpc::session_update("session-1", AcpMessage::agent_text("answer"))
        )
        .expect("answer update should be written");

        assert_eq!(output.rendered(), "[think]\nthinking\n[answer]\nanswer");
    }

    /// Verifies filesystem tools are visible through the default tool router.
    #[tokio::test]
    async fn router_exposes_acp_filesystem_tools() {
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let file_path = workspace.path().join("tool-created.txt");
        let router = tools::create_file_tool_router_with_root(workspace.path()).await;

        let definitions = router.definitions().await;
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
