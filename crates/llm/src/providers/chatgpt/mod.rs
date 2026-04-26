use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use http::HeaderValue;
use snafu::ResultExt;

use crate::{
    client::ProviderClient,
    client::{self, ApiKey, Capabilities, Capable, DebugExt, Nothing, Provider, ProviderBuilder},
    completion::{
        AssistantContent, ClientSnafu, CompletionError, ProviderSnafu, message::ReasoningContent,
    },
    http_client::{self, HttpClientExt},
    one_or_many::OneOrMany,
    providers::openai::{
        self,
        codex::{OPENAI_CODEX_API_BASE_URL, OPENAI_CODEX_AUTH_ENDPOINT, OPENAI_CODEX_CLIENT_ID},
        responses_api,
    },
    usage::Usage,
};

pub mod auth;
pub mod ws;

pub use ws::WsCompletionModel;

const DEFAULT_ORIGINATOR: &str = "clawcode";
const DEFAULT_INSTRUCTIONS: &str = "You are ChatGPT, a helpful AI assistant.";

/// `gpt-5.4`
pub const GPT_5_4: &str = "gpt-5.4";
/// `gpt-5.3-codex`
pub const GPT_5_3_CODEX: &str = "gpt-5.3-codex";
/// `gpt-5.3-codex-spark`
pub const GPT_5_3_CODEX_SPARK: &str = "gpt-5.3-codex-spark";

/// Authentication modes supported by the ChatGPT provider.
#[derive(Clone)]
pub enum ChatGPTAuth {
    AccessToken { access_token: String },
    OAuth,
}

impl std::fmt::Debug for ChatGPTAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccessToken { .. } => f.write_str("AccessToken(<redacted>)"),
            Self::OAuth => f.write_str("OAuth"),
        }
    }
}

impl ApiKey for ChatGPTAuth {}

impl<S> From<S> for ChatGPTAuth
where
    S: Into<String>,
{
    fn from(value: S) -> Self {
        Self::AccessToken {
            access_token: value.into(),
        }
    }
}

/// Builder configuration for the ChatGPT provider extension.
#[derive(Debug, Clone)]
pub struct ChatGPTBuilder {
    auth_config: auth::AuthConfig,
    default_instructions: Option<String>,
    originator: String,
    user_agent: Option<String>,
}

impl Default for ChatGPTBuilder {
    /// Builds the default ChatGPT provider configuration from environment variables.
    fn default() -> Self {
        Self {
            auth_config: auth::AuthConfig {
                auth_endpoint: std::env::var("CHATGPT_AUTH_BASE")
                    .unwrap_or_else(|_| OPENAI_CODEX_AUTH_ENDPOINT.to_string()),
                api_base_url: std::env::var("CHATGPT_API_BASE")
                    .or_else(|_| std::env::var("OPENAI_CHATGPT_API_BASE"))
                    .unwrap_or_else(|_| OPENAI_CODEX_API_BASE_URL.to_string()),
                client_id: std::env::var("CHATGPT_CLIENT_ID")
                    .unwrap_or_else(|_| OPENAI_CODEX_CLIENT_ID.to_string()),
                session_path: default_auth_file(),
                token_refresh_margin_secs: 300,
            },
            default_instructions: Some(
                std::env::var("CHATGPT_DEFAULT_INSTRUCTIONS")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_INSTRUCTIONS.to_string()),
            ),
            originator: std::env::var("CHATGPT_ORIGINATOR")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_ORIGINATOR.to_string()),
            user_agent: std::env::var("CHATGPT_USER_AGENT")
                .ok()
                .filter(|value| !value.is_empty()),
        }
    }
}

/// Provider extension stored inside the generic client wrapper.
#[derive(Clone)]
pub struct ChatGPTExt {
    auth: auth::Authenticator,
    default_instructions: Option<String>,
    originator: String,
    user_agent: Option<String>,
}

impl std::fmt::Debug for ChatGPTExt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatGPTExt")
            .field("auth", &self.auth)
            .field("default_instructions", &self.default_instructions)
            .field("originator", &self.originator)
            .field("user_agent", &self.user_agent)
            .finish()
    }
}

pub type Client<H = reqwest::Client> = client::Client<ChatGPTExt, H>;
pub type ClientBuilder<H = reqwest::Client> = client::ClientBuilder<ChatGPTBuilder, ChatGPTAuth, H>;

impl Provider for ChatGPTExt {
    type Builder = ChatGPTBuilder;
    const VERIFY_PATH: &'static str = "";
}

impl<H> Capabilities<H> for ChatGPTExt {
    type Completion = Capable<ResponsesCompletionModel<H>>;
    type Embeddings = Nothing;
    type Transcription = Nothing;
    type ModelListing = Nothing;
}

impl DebugExt for ChatGPTExt {}

impl ProviderBuilder for ChatGPTBuilder {
    type Extension<H>
        = ChatGPTExt
    where
        H: HttpClientExt;
    type ApiKey = ChatGPTAuth;

    const BASE_URL: &'static str = OPENAI_CODEX_API_BASE_URL;

    fn build<H>(
        builder: &client::ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<Self::Extension<H>>
    where
        H: HttpClientExt,
    {
        let auth_source = match builder.get_api_key() {
            ChatGPTAuth::AccessToken { access_token } => auth::AuthSource::AccessToken {
                access_token: access_token.clone(),
            },
            ChatGPTAuth::OAuth => auth::AuthSource::OAuth,
        };

        Ok(ChatGPTExt {
            auth: auth::Authenticator::new(auth_source, builder.ext().auth_config.clone()),
            default_instructions: builder.ext().default_instructions.clone(),
            originator: builder.ext().originator.clone(),
            user_agent: builder.ext().user_agent.clone(),
        })
    }
}

impl ProviderClient for Client {
    type Input = ChatGPTAuth;

    fn from_env() -> Self {
        let builder = Self::builder();
        if let Ok(access_token) = std::env::var("CHATGPT_ACCESS_TOKEN") {
            builder
                .api_key(ChatGPTAuth::AccessToken { access_token })
                .build()
                .unwrap()
        } else {
            builder.oauth().build().unwrap()
        }
    }

    fn from_val(input: Self::Input) -> Self {
        Self::builder().api_key(input).build().unwrap()
    }
}

impl<H> client::ClientBuilder<ChatGPTBuilder, client::NeedsApiKey, H> {
    /// Switches the ChatGPT provider to persisted OAuth device-code authentication.
    pub fn oauth(self) -> client::ClientBuilder<ChatGPTBuilder, ChatGPTAuth, H> {
        self.api_key(ChatGPTAuth::OAuth)
    }
}

impl<H> ClientBuilder<H> {
    /// Overrides the persisted OAuth session file used by the ChatGPT provider.
    pub fn auth_file(self, path: impl AsRef<Path>) -> Self {
        let auth_file = path.as_ref().to_path_buf();
        self.over_ext(|mut ext| {
            ext.auth_config.session_path = auth_file;
            ext
        })
    }

    /// Overrides the originator header sent to the ChatGPT backend.
    pub fn originator(self, originator: impl Into<String>) -> Self {
        let originator = originator.into();
        self.over_ext(|mut ext| {
            ext.originator = originator;
            ext
        })
    }

    /// Overrides the user-agent header sent to the ChatGPT backend.
    pub fn user_agent(self, user_agent: impl Into<String>) -> Self {
        let user_agent = user_agent.into();
        self.over_ext(|mut ext| {
            ext.user_agent = Some(user_agent);
            ext
        })
    }

    /// Overrides the default instructions injected into ChatGPT requests.
    pub fn default_instructions(self, instructions: impl Into<String>) -> Self {
        let instructions = instructions.into();
        self.over_ext(|mut ext| {
            ext.default_instructions = Some(instructions);
            ext
        })
    }
}

impl<H> Client<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    /// Creates a ChatGPT websocket session builder for the requested model.
    pub fn responses_websocket_builder(
        &self,
        model: impl Into<String>,
    ) -> ResponsesWebSocketSessionBuilder<H> {
        ResponsesWebSocketSessionBuilder::new(ResponsesCompletionModel::new(self.clone(), model))
    }

    /// Connects a ChatGPT websocket session for the requested model.
    pub async fn responses_websocket(
        &self,
        model: impl Into<String>,
    ) -> Result<ResponsesWebSocketSession<H>, CompletionError> {
        self.responses_websocket_builder(model).connect().await
    }
}

/// ChatGPT-backed responses model that reuses the existing OpenAI Responses payloads and parser.
#[derive(Clone)]
pub struct ResponsesCompletionModel<H = reqwest::Client> {
    client: Client<H>,
    pub model: String,
    pub tools: Vec<responses_api::ResponsesToolDefinition>,
}

impl<H> ResponsesCompletionModel<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    /// Creates a new ChatGPT responses model for the provided client and model name.
    pub fn new(client: Client<H>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            tools: Vec::new(),
        }
    }

    /// Attaches one default tool definition to all ChatGPT responses requests.
    pub fn with_tool(mut self, tool: impl Into<responses_api::ResponsesToolDefinition>) -> Self {
        self.tools.push(tool.into());
        self
    }

    /// Attaches multiple default tool definitions to all ChatGPT responses requests.
    pub fn with_tools<I, Tool>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = Tool>,
        Tool: Into<responses_api::ResponsesToolDefinition>,
    {
        self.tools.extend(tools.into_iter().map(Into::into));
        self
    }

    /// Injects provider-default instructions so ChatGPT requests always satisfy the backend contract.
    fn prepare_completion_request(
        &self,
        mut completion_request: crate::completion::CompletionRequest,
    ) -> crate::completion::CompletionRequest {
        if let Some(default_instructions) = &self.client.ext().default_instructions {
            completion_request.preamble = Some(merge_instructions(
                default_instructions,
                completion_request.preamble.as_deref(),
            ));
        }
        completion_request
    }

    /// Creates a websocket session builder for this ChatGPT responses model.
    pub fn websocket_session_builder(&self) -> ResponsesWebSocketSessionBuilder<H> {
        ResponsesWebSocketSessionBuilder::new(self.clone())
    }

    /// Opens a websocket session for this ChatGPT responses model.
    pub async fn websocket_session(&self) -> Result<ResponsesWebSocketSession<H>, CompletionError> {
        self.websocket_session_builder().connect().await
    }

    /// Builds a temporary OpenAI responses model with ChatGPT auth headers injected.
    async fn openai_model(
        &self,
    ) -> Result<openai::responses_api::ResponsesCompletionModel<H>, CompletionError> {
        let auth = self
            .client
            .ext()
            .auth
            .auth_context()
            .await
            .map_err(|error| CompletionError::Provider {
                msg: error.to_string(),
            })?;
        let mut headers = self.client.headers().clone();
        headers.insert(
            http::HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(&auth.account_id).map_err(|error| CompletionError::Provider {
                msg: error.to_string(),
            })?,
        );
        headers.insert(
            http::HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(
            http::HeaderName::from_static("originator"),
            HeaderValue::from_str(&self.client.ext().originator).map_err(|error| {
                CompletionError::Provider {
                    msg: error.to_string(),
                }
            })?,
        );
        if let Some(user_agent) = &self.client.ext().user_agent {
            headers.insert(
                http::header::USER_AGENT,
                HeaderValue::from_str(user_agent).map_err(|error| CompletionError::Provider {
                    msg: error.to_string(),
                })?,
            );
        }

        let client = openai::Client::builder()
            .base_url(self.client.base_url())
            .http_client(self.client.http_client().clone())
            .http_headers(headers)
            .api_key(auth.access_token)
            .build()
            .context(ClientSnafu {
                stage: "chatgpt-build-openai-client".to_string(),
            })?;

        let mut model =
            openai::responses_api::ResponsesCompletionModel::with_model(client, &self.model);
        model.tools = self.tools.clone();
        Ok(model)
    }
}

/// Aggregated raw response returned by ChatGPT completion mode.
///
/// ChatGPT/Codex currently requires streaming requests, so blocking completions
/// are reconstructed from the streamed terminal usage and message identifier.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ChatGPTCompletionResponse {
    /// Token usage reported by the streamed ChatGPT response when available.
    pub usage: crate::usage::Usage,
    /// Provider-assigned message identifier captured from streamed output items.
    pub message_id: Option<String>,
}

/// Configures and opens a ChatGPT websocket session backed by the Responses API.
pub struct ResponsesWebSocketSessionBuilder<H = reqwest::Client> {
    model: ResponsesCompletionModel<H>,
    connect_timeout: Option<Duration>,
    event_timeout: Option<Duration>,
}

impl<H> ResponsesWebSocketSessionBuilder<H> {
    /// Creates a websocket session builder for the provided ChatGPT model.
    pub fn new(model: ResponsesCompletionModel<H>) -> Self {
        Self {
            model,
            connect_timeout: Some(Duration::from_secs(30)),
            event_timeout: None,
        }
    }

    /// Sets the websocket connection timeout.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Disables the websocket connection timeout.
    #[must_use]
    pub fn without_connect_timeout(mut self) -> Self {
        self.connect_timeout = None;
        self
    }

    /// Sets the timeout for waiting on the next websocket event.
    #[must_use]
    pub fn event_timeout(mut self, timeout: Duration) -> Self {
        self.event_timeout = Some(timeout);
        self
    }

    /// Disables the per-event websocket timeout.
    #[must_use]
    pub fn without_event_timeout(mut self) -> Self {
        self.event_timeout = None;
        self
    }
}

impl<H> ResponsesWebSocketSessionBuilder<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    /// Connects the ChatGPT websocket session with the configured timeouts.
    pub async fn connect(self) -> Result<ResponsesWebSocketSession<H>, CompletionError> {
        let mut builder = responses_api::websocket::ResponsesWebSocketSessionBuilder::new(
            self.model.openai_model().await?,
        );
        builder = match self.connect_timeout {
            Some(timeout) => builder.connect_timeout(timeout),
            None => builder.without_connect_timeout(),
        };
        builder = match self.event_timeout {
            Some(timeout) => builder.event_timeout(timeout),
            None => builder.without_event_timeout(),
        };

        Ok(ResponsesWebSocketSession {
            model: self.model,
            inner: builder.connect().await?,
        })
    }
}

/// A ChatGPT websocket session that injects provider defaults into each turn.
pub struct ResponsesWebSocketSession<H = reqwest::Client> {
    model: ResponsesCompletionModel<H>,
    inner: responses_api::websocket::ResponsesWebSocketSession<H>,
}

impl<H> crate::completion::CompletionModel for ResponsesCompletionModel<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    type Response = ChatGPTCompletionResponse;
    type StreamingResponse = responses_api::streaming::StreamingCompletionResponse;
    type Client = Client<H>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self::new(client.clone(), model)
    }

    async fn completion(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<crate::completion::CompletionResponse<Self::Response>, CompletionError> {
        // ChatGPT/Codex rejects non-streaming responses requests, so blocking
        // completions are collected by draining the streaming response.
        let mut stream = self.stream(completion_request).await?;
        while stream.next().await.transpose()?.is_some() {}

        let usage = stream
            .response
            .as_ref()
            .and_then(crate::usage::GetTokenUsage::token_usage)
            .unwrap_or_default();
        let message_id = stream.message_id.clone();

        Ok(crate::completion::CompletionResponse {
            choice: stream.choice,
            usage,
            raw_response: ChatGPTCompletionResponse { usage, message_id },
            message_id: stream.message_id,
        })
    }

    async fn stream(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<
        crate::streaming::StreamingCompletionResponse<Self::StreamingResponse>,
        CompletionError,
    > {
        self.openai_model()
            .await?
            .stream(self.prepare_completion_request(completion_request))
            .await
    }
}

impl<H> ResponsesWebSocketSession<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    /// Returns the previous successful response ID tracked by the websocket chain.
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        self.inner.previous_response_id()
    }

    /// Clears the cached previous response ID so the next turn starts a fresh chain.
    pub fn clear_previous_response_id(&mut self) {
        self.inner.clear_previous_response_id();
    }

    /// Sends a ChatGPT websocket turn after injecting default instructions.
    pub async fn send(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<(), CompletionError> {
        self.inner
            .send(self.model.prepare_completion_request(completion_request))
            .await
    }

    /// Sends a websocket turn with explicit create options.
    pub async fn send_with_options(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
        options: responses_api::websocket::ResponsesWebSocketCreateOptions,
    ) -> Result<(), CompletionError> {
        self.inner
            .send_with_options(
                self.model.prepare_completion_request(completion_request),
                options,
            )
            .await
    }

    /// Reads the next websocket event for the current in-flight ChatGPT turn.
    pub async fn next_event(
        &mut self,
    ) -> Result<responses_api::websocket::ResponsesWebSocketEvent, CompletionError> {
        self.inner.next_event().await
    }

    /// Sends a warmup turn and returns the resulting response ID.
    pub async fn warmup(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<String, CompletionError> {
        self.inner
            .warmup(self.model.prepare_completion_request(completion_request))
            .await
    }

    /// Collects one completed websocket turn into a blocking ChatGPT completion response.
    pub async fn completion(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<crate::completion::CompletionResponse<ChatGPTCompletionResponse>, CompletionError>
    {
        self.send(completion_request).await?;
        let mut state = WebSocketCompletionState::default();

        loop {
            match self.next_event().await? {
                responses_api::websocket::ResponsesWebSocketEvent::Item(item) => {
                    state.record_item(item.data);
                }
                responses_api::websocket::ResponsesWebSocketEvent::Response(chunk) => {
                    if matches!(
                        chunk.kind,
                        responses_api::streaming::ResponseChunkKind::ResponseCompleted
                            | responses_api::streaming::ResponseChunkKind::ResponseFailed
                            | responses_api::streaming::ResponseChunkKind::ResponseIncomplete
                    ) {
                        let response = validate_terminal_response(chunk.response)?;
                        state.record_terminal_response(&response);
                        return Ok(state.into_completion_response());
                    }
                }
                responses_api::websocket::ResponsesWebSocketEvent::Done(done) => {
                    let response =
                        serde_json::from_value(done.response.clone()).map_err(|error| {
                            CompletionError::Provider {
                                msg: format!(
                                    "Failed to decode ChatGPT websocket terminal response: {error}"
                                ),
                            }
                        })?;
                    let response = validate_terminal_response(response)?;
                    state.record_terminal_response(&response);
                    return Ok(state.into_completion_response());
                }
                responses_api::websocket::ResponsesWebSocketEvent::Error(error) => {
                    return ProviderSnafu {
                        msg: error.to_string(),
                    }
                    .fail();
                }
            }
        }
    }

    /// Closes the underlying websocket connection cleanly.
    pub async fn close(&mut self) -> Result<(), CompletionError> {
        self.inner.close().await
    }
}

/// Returns the default location of the persisted ChatGPT OAuth session file.
fn default_auth_file() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("auth.json")
}

/// Merges provider-default instructions with an optional request-specific preamble.
fn merge_instructions(default_instructions: &str, existing_instructions: Option<&str>) -> String {
    match existing_instructions
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(existing) if existing.contains(default_instructions) => existing.to_string(),
        Some(existing) => format!("{default_instructions}\n\n{existing}"),
        None => default_instructions.to_string(),
    }
}

/// Tracks websocket completion state so blocking ChatGPT completions can be rebuilt from events.
#[derive(Default)]
struct WebSocketCompletionState {
    assistant_items: Vec<AssistantContent>,
    text_buffer: String,
    message_id: Option<String>,
    usage: Usage,
}

impl WebSocketCompletionState {
    /// Records one websocket item event into the aggregated completion state.
    fn record_item(&mut self, item: responses_api::streaming::ItemChunkKind) {
        match item {
            responses_api::streaming::ItemChunkKind::OutputTextDelta(delta) => {
                self.text_buffer.push_str(&delta.delta);
            }
            responses_api::streaming::ItemChunkKind::RefusalDelta(delta) => {
                self.text_buffer.push_str(&delta.delta);
            }
            responses_api::streaming::ItemChunkKind::OutputItemDone(done) => {
                self.record_output(done.item);
            }
            _ => {}
        }
    }

    /// Records final response metadata and falls back to terminal output when deltas were absent.
    fn record_terminal_response(&mut self, response: &responses_api::CompletionResponse) {
        self.usage = response
            .usage
            .as_ref()
            .map_or_else(Usage::new, response_usage);

        if self.message_id.is_none() {
            self.message_id = response.output.iter().find_map(|item| match item {
                responses_api::Output::Message(message) => Some(message.id.clone()),
                _ => None,
            });
        }

        if self.assistant_items.is_empty() && self.text_buffer.is_empty() {
            for item in response.output.iter().cloned() {
                self.record_output(item);
            }
        }
    }

    /// Converts the aggregated websocket state into the blocking completion response shape.
    fn into_completion_response(
        mut self,
    ) -> crate::completion::CompletionResponse<ChatGPTCompletionResponse> {
        self.flush_text();
        if self.assistant_items.is_empty() {
            self.assistant_items.push(AssistantContent::text(""));
        }

        crate::completion::CompletionResponse {
            choice: OneOrMany::many(self.assistant_items)
                .expect("websocket aggregation always yields at least one assistant item"),
            usage: self.usage,
            raw_response: ChatGPTCompletionResponse {
                usage: self.usage,
                message_id: self.message_id.clone(),
            },
            message_id: self.message_id,
        }
    }

    /// Records a completed output item while preserving the relative order of text vs non-text items.
    fn record_output(&mut self, output: responses_api::Output) {
        match output {
            responses_api::Output::Message(message) => {
                self.message_id = Some(message.id);
            }
            responses_api::Output::FunctionCall(function_call) => {
                self.flush_text();
                self.assistant_items
                    .push(AssistantContent::tool_call_with_call_id(
                        function_call.id,
                        function_call.call_id,
                        function_call.name,
                        function_call.arguments,
                    ));
            }
            responses_api::Output::Reasoning {
                id,
                summary,
                encrypted_content,
                ..
            } => {
                self.flush_text();
                let mut content = summary
                    .into_iter()
                    .map(|summary| match summary {
                        responses_api::ReasoningSummary::SummaryText { text } => {
                            ReasoningContent::Summary(text)
                        }
                    })
                    .collect::<Vec<_>>();
                if let Some(encrypted) = encrypted_content {
                    content.push(ReasoningContent::Encrypted(encrypted));
                }
                self.assistant_items.push(AssistantContent::Reasoning(
                    crate::completion::message::Reasoning {
                        id: Some(id),
                        content,
                    },
                ));
            }
        }
    }

    /// Flushes buffered text before non-text items so assistant output ordering stays stable.
    fn flush_text(&mut self) {
        if !self.text_buffer.is_empty() {
            self.assistant_items
                .push(AssistantContent::text(std::mem::take(
                    &mut self.text_buffer,
                )));
        }
    }
}

/// Converts provider usage into the shared usage type consumed by upper layers.
fn response_usage(usage: &responses_api::ResponsesUsage) -> Usage {
    let mut normalized = Usage::new();
    normalized.input_tokens = usage.input_tokens;
    normalized.output_tokens = usage.output_tokens;
    normalized.total_tokens = usage.total_tokens;
    normalized.cached_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map(|details| details.cached_tokens)
        .unwrap_or(0);
    normalized
}

/// Validates that a terminal websocket response ended successfully before exposing it upstream.
fn validate_terminal_response(
    response: responses_api::CompletionResponse,
) -> Result<responses_api::CompletionResponse, CompletionError> {
    match response.status {
        responses_api::ResponseStatus::Completed => Ok(response),
        responses_api::ResponseStatus::Failed => ProviderSnafu {
            msg: response_error_message(response.error.as_ref(), "failed response"),
        }
        .fail(),
        responses_api::ResponseStatus::Incomplete => {
            let reason = response
                .incomplete_details
                .as_ref()
                .map(|details| details.reason.as_str())
                .unwrap_or("unknown reason");
            ProviderSnafu {
                msg: format!("ChatGPT websocket response was incomplete: {reason}"),
            }
            .fail()
        }
        status => ProviderSnafu {
            msg: format!("ChatGPT websocket response ended with status {:?}", status),
        }
        .fail(),
    }
}

/// Formats provider failure payloads into the concise error strings used by websocket completion.
fn response_error_message(error: Option<&responses_api::ResponseError>, fallback: &str) -> String {
    if let Some(error) = error {
        if error.code.is_empty() {
            error.message.clone()
        } else {
            format!("{}: {}", error.code, error.message)
        }
    } else {
        format!("ChatGPT websocket returned a {fallback}")
    }
}
