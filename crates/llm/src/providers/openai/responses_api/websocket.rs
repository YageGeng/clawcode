//! WebSocket session support for the OpenAI Responses API.
//!
//! This module implements OpenAI's `/v1/responses` WebSocket mode as a stateful,
//! sequential session. Each connection supports a single in-flight response at a
//! time, which matches OpenAI's current protocol constraints.

use crate::completion::{self, CompletionError, ProviderSnafu, SerializeSnafu, UrlSnafu};
use crate::http_client::HttpClientExt;
use crate::providers::openai::responses_api::streaming::{
    ItemChunk, ResponseChunk, ResponseChunkKind, StreamingCompletionChunk,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use snafu::ResultExt;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{self, Message, client::IntoClientRequest},
};
use tracing::Level;
use url::Url;

use super::{CompletionResponse, ResponseError, ResponseStatus, ResponsesCompletionModel};

type OpenAIWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Options for a `response.create` message sent over OpenAI WebSocket mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponsesWebSocketCreateOptions {
    /// When set to `false`, OpenAI prepares request state without generating a model output.
    ///
    /// This is the "warmup" mode described in the OpenAI WebSocket mode guide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate: Option<bool>,
}

impl ResponsesWebSocketCreateOptions {
    /// Creates warmup options equivalent to `generate: false`.
    #[must_use]
    pub fn warmup() -> Self {
        Self {
            generate: Some(false),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ResponsesWebSocketClientEvent {
    #[serde(rename = "type")]
    kind: ResponsesWebSocketClientEventKind,
    #[serde(flatten)]
    request: super::CompletionRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
enum ResponsesWebSocketClientEventKind {
    #[serde(rename = "response.create")]
    ResponseCreate,
}

/// A protocol error event emitted by OpenAI WebSocket mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesWebSocketErrorEvent {
    /// The event type.
    #[serde(rename = "type")]
    pub kind: ResponsesWebSocketErrorEventKind,
    /// The provider error payload.
    pub error: ResponsesWebSocketErrorPayload,
}

impl std::fmt::Display for ResponsesWebSocketErrorEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

/// The event kind for an OpenAI WebSocket protocol error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponsesWebSocketErrorEventKind {
    #[serde(rename = "error")]
    Error,
}

/// The payload carried by an OpenAI WebSocket protocol error event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponsesWebSocketErrorPayload {
    /// Provider-specific error code when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Human-readable error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Any extra fields supplied by the provider.
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl std::fmt::Display for ResponsesWebSocketErrorPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.code, &self.message) {
            (Some(code), Some(message)) => write!(f, "{code}: {message}"),
            (None, Some(message)) => f.write_str(message),
            (Some(code), None) => f.write_str(code),
            (None, None) => f.write_str("OpenAI websocket error"),
        }
    }
}

/// The optional `response.done` event emitted by OpenAI WebSocket mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesWebSocketDoneEvent {
    /// The event type.
    #[serde(rename = "type")]
    pub kind: ResponsesWebSocketDoneEventKind,
    /// The provider payload for the finished response.
    pub response: Value,
}

impl ResponsesWebSocketDoneEvent {
    /// Returns the response ID if the payload includes one.
    #[must_use]
    pub fn response_id(&self) -> Option<&str> {
        self.response.get("id").and_then(Value::as_str)
    }

    fn status(&self) -> Option<ResponseStatus> {
        self.response
            .get("status")
            .cloned()
            .and_then(|status| serde_json::from_value(status).ok())
    }

    fn as_completion_response(&self) -> Option<CompletionResponse> {
        serde_json::from_value(self.response.clone()).ok()
    }
}

/// The event kind for the terminal websocket event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponsesWebSocketDoneEventKind {
    #[serde(rename = "response.done")]
    ResponseDone,
}

/// A server event emitted by OpenAI WebSocket mode.
#[derive(Debug, Clone)]
pub enum ResponsesWebSocketEvent {
    /// A response lifecycle event such as `response.created` or `response.completed`.
    Response(Box<ResponseChunk>),
    /// A streaming item/delta event such as `response.output_text.delta`.
    Item(ItemChunk),
    /// A protocol-level websocket error event.
    Error(ResponsesWebSocketErrorEvent),
    /// An optional `response.done` event emitted by OpenAI over WebSockets.
    Done(ResponsesWebSocketDoneEvent),
}

impl ResponsesWebSocketEvent {
    /// Returns the response ID when the event includes one.
    #[must_use]
    pub fn response_id(&self) -> Option<&str> {
        match self {
            Self::Response(chunk) => Some(&chunk.response.id),
            Self::Done(done) => done.response_id(),
            Self::Item(_) | Self::Error(_) => None,
        }
    }

    /// Returns `true` when this event ends the current in-flight websocket turn.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Response(chunk) => matches!(
                chunk.kind,
                ResponseChunkKind::ResponseCompleted
                    | ResponseChunkKind::ResponseFailed
                    | ResponseChunkKind::ResponseIncomplete
            ),
            Self::Error(_) | Self::Done(_) => true,
            Self::Item(_) => false,
        }
    }
}

/// A builder for an OpenAI Responses WebSocket session.
///
/// The default builder applies a 30 second connection timeout and leaves the
/// per-event timeout disabled.
pub struct ResponsesWebSocketSessionBuilder<H = reqwest::Client> {
    model: ResponsesCompletionModel<H>,
    connect_timeout: Option<Duration>,
    event_timeout: Option<Duration>,
}

impl<H> ResponsesWebSocketSessionBuilder<H> {
    pub fn new(model: ResponsesCompletionModel<H>) -> Self {
        Self {
            model,
            connect_timeout: Some(DEFAULT_CONNECT_TIMEOUT),
            event_timeout: None,
        }
    }

    /// Sets the timeout for establishing the websocket connection.
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

    /// Disables the websocket event timeout.
    #[must_use]
    pub fn without_event_timeout(mut self) -> Self {
        self.event_timeout = None;
        self
    }
}

impl<H> ResponsesWebSocketSessionBuilder<H>
where
    H: HttpClientExt + Clone + std::fmt::Debug + Default + Send + Sync + 'static,
{
    /// Opens the websocket session using the configured builder options.
    pub async fn connect(self) -> Result<ResponsesWebSocketSession<H>, CompletionError> {
        ResponsesWebSocketSession::connect_with_timeouts(
            self.model,
            self.connect_timeout,
            self.event_timeout,
        )
        .await
    }
}

/// A stateful OpenAI Responses WebSocket session.
///
/// This session keeps track of the most recent successful `response.id` so later
/// turns can automatically chain via `previous_response_id` unless the request
/// explicitly sets a different one.
///
/// Call [`ResponsesWebSocketSession::close`] when you are finished with the
/// session so the websocket can complete a close handshake cleanly.
pub struct ResponsesWebSocketSession<H = reqwest::Client> {
    model: ResponsesCompletionModel<H>,
    previous_response_id: Option<String>,
    pending_done_response_id: Option<String>,
    socket: OpenAIWebSocket,
    in_flight: bool,
    event_timeout: Option<Duration>,
    closed: bool,
    failed: bool,
}

impl<H> ResponsesWebSocketSession<H>
where
    H: HttpClientExt + Clone + std::fmt::Debug + Default + Send + Sync + 'static,
{
    async fn connect_with_timeouts(
        model: ResponsesCompletionModel<H>,
        connect_timeout: Option<Duration>,
        event_timeout: Option<Duration>,
    ) -> Result<Self, CompletionError> {
        let url = websocket_url(model.client.base_url())?;
        let request = websocket_request(&url, model.client.headers())?;
        let socket = connect_websocket(request, connect_timeout).await?;

        Ok(Self {
            model,
            previous_response_id: None,
            pending_done_response_id: None,
            socket,
            in_flight: false,
            event_timeout,
            closed: false,
            failed: false,
        })
    }

    /// Returns the most recent successful `response.id` tracked by this session.
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        self.previous_response_id.as_deref()
    }

    /// Clears the cached `previous_response_id` so the next turn starts a fresh chain.
    pub fn clear_previous_response_id(&mut self) {
        self.previous_response_id = None;
    }

    /// Sends a `response.create` event for a Rig completion request.
    pub async fn send(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<(), CompletionError> {
        self.send_with_options(
            completion_request,
            ResponsesWebSocketCreateOptions::default(),
        )
        .await
    }

    /// Sends a `response.create` event with explicit websocket-mode options.
    pub async fn send_with_options(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
        options: ResponsesWebSocketCreateOptions,
    ) -> Result<(), CompletionError> {
        self.ensure_open()?;

        if self.in_flight {
            return ProviderSnafu {
                msg: "An OpenAI websocket response is already in flight on this session",
            }
            .fail();
        }

        let payload = ResponsesWebSocketClientEvent {
            kind: ResponsesWebSocketClientEventKind::ResponseCreate,
            request: self.prepare_request(completion_request)?,
            generate: options.generate,
        };

        if tracing::enabled!(Level::TRACE) {
            tracing::trace!(
                target: "rig::completions",
                "OpenAI websocket request: {}",
                serde_json::to_string_pretty(&payload).context(SerializeSnafu {stage: "request-tracing"})?
            );
        }

        let payload = serde_json::to_string(&payload).context(SerializeSnafu {
            stage: "request-serialize",
        })?;

        if let Err(error) = self.socket.send(Message::text(payload)).await {
            return Err(self.fail_session(websocket_provider_error(error)));
        }
        self.in_flight = true;

        Ok(())
    }

    /// Reads the next server event for the current in-flight turn.
    pub async fn next_event(&mut self) -> Result<ResponsesWebSocketEvent, CompletionError> {
        self.ensure_open()?;

        if !self.in_flight {
            return ProviderSnafu {
                msg: "No OpenAI websocket response is currently in flight on this session",
            }
            .fail();
        }

        loop {
            let message = match self.read_next_message().await {
                Ok(message) => message,
                Err(error) => return Err(error),
            };

            let Some(message) = message else {
                self.mark_closed();
                return ProviderSnafu {
                    msg: "The OpenAI websocket connection closed before the turn finished",
                }
                .fail();
            };

            let message = match message {
                Ok(message) => message,
                Err(error) => return Err(self.fail_session(websocket_provider_error(error))),
            };
            let payload = match websocket_message_to_text(message) {
                Ok(Some(payload)) => payload,
                Ok(None) => continue,
                Err(error) => return Err(self.fail_session(error)),
            };
            let event = match parse_server_event(&payload) {
                Ok(Some(event)) => event,
                Ok(None) => continue,
                Err(error) => return Err(self.fail_session(error)),
            };
            if let ResponsesWebSocketEvent::Done(done) = &event {
                // OpenAI may emit `response.done` after the turn has already ended at
                // `response.completed`. Ignore that trailing event on the next turn.
                if self.pending_done_response_id.as_deref() == done.response_id() {
                    self.pending_done_response_id = None;
                    continue;
                }
            }
            self.update_state_for_event(&event);
            return Ok(event);
        }
    }

    /// Sends a warmup turn (`generate: false`) and returns the resulting response ID.
    pub async fn warmup(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<String, CompletionError> {
        self.send_with_options(
            completion_request,
            ResponsesWebSocketCreateOptions::warmup(),
        )
        .await?;
        let response = self.wait_for_completed_response().await?;
        Ok(response.id)
    }

    /// Sends a completion turn and collects the final OpenAI response.
    pub async fn completion(
        &mut self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<completion::CompletionResponse<CompletionResponse>, CompletionError> {
        self.send(completion_request).await?;
        let response = self.wait_for_completed_response().await?;
        response.try_into()
    }

    /// Closes the websocket connection.
    ///
    /// Call this when you are finished with the session so the websocket can
    /// terminate with a clean close handshake.
    pub async fn close(&mut self) -> Result<(), CompletionError> {
        if self.closed {
            return Ok(());
        }

        let result = self
            .socket
            .close(None)
            .await
            .map_err(websocket_provider_error);
        self.mark_closed();
        result
    }

    fn prepare_request(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<super::CompletionRequest, CompletionError> {
        let mut request = self.model.create_completion_request(completion_request)?;

        // WebSocket mode is always event-driven, so these HTTP/SSE-specific flags
        // are ignored by the provider and only add noise to the payload.
        request.stream = None;
        request.additional_parameters.background = None;

        if request.additional_parameters.previous_response_id.is_none() {
            request.additional_parameters.previous_response_id = self.previous_response_id.clone();
        }

        Ok(request)
    }

    async fn wait_for_completed_response(&mut self) -> Result<CompletionResponse, CompletionError> {
        loop {
            match self.next_event().await? {
                ResponsesWebSocketEvent::Response(chunk) => {
                    if matches!(
                        chunk.kind,
                        ResponseChunkKind::ResponseCompleted
                            | ResponseChunkKind::ResponseFailed
                            | ResponseChunkKind::ResponseIncomplete
                    ) {
                        return terminal_response_result(chunk.response);
                    }
                }
                ResponsesWebSocketEvent::Done(done) => {
                    if let Some(response) = done.as_completion_response() {
                        return terminal_response_result(response);
                    }

                    let message = if let Some(response_id) = done.response_id() {
                        format!(
                            "OpenAI websocket turn ended with response.done before a terminal response body was available (response_id={response_id})"
                        )
                    } else {
                        "OpenAI websocket turn ended with response.done before a terminal response body was available"
                            .to_string()
                    };

                    return ProviderSnafu { msg: message }.fail();
                }
                ResponsesWebSocketEvent::Error(error) => {
                    return ProviderSnafu {
                        msg: error.to_string(),
                    }
                    .fail();
                }
                ResponsesWebSocketEvent::Item(_) => {}
            }
        }
    }

    fn update_state_for_event(&mut self, event: &ResponsesWebSocketEvent) {
        match event {
            ResponsesWebSocketEvent::Response(chunk) => match chunk.kind {
                ResponseChunkKind::ResponseCompleted => {
                    let response_id = chunk.response.id.clone();
                    self.previous_response_id = Some(response_id.clone());
                    self.pending_done_response_id = Some(response_id);
                    self.in_flight = false;
                }
                ResponseChunkKind::ResponseFailed | ResponseChunkKind::ResponseIncomplete => {
                    self.pending_done_response_id = Some(chunk.response.id.clone());
                    self.previous_response_id = None;
                    self.in_flight = false;
                }
                ResponseChunkKind::ResponseCreated | ResponseChunkKind::ResponseInProgress => {}
            },
            ResponsesWebSocketEvent::Done(done) => {
                match done.status() {
                    Some(ResponseStatus::Completed) => {
                        if let Some(response_id) = done.response_id() {
                            self.previous_response_id = Some(response_id.to_string());
                        }
                    }
                    Some(ResponseStatus::Failed)
                    | Some(ResponseStatus::Incomplete)
                    | Some(ResponseStatus::Cancelled) => {
                        self.previous_response_id = None;
                    }
                    Some(ResponseStatus::InProgress | ResponseStatus::Queued) | None => {}
                }
                self.pending_done_response_id = None;
                self.in_flight = false;
            }
            ResponsesWebSocketEvent::Error(_) => {
                self.previous_response_id = None;
                self.pending_done_response_id = None;
                self.in_flight = false;
            }
            ResponsesWebSocketEvent::Item(_) => {}
        }
    }

    fn abort_turn(&mut self) {
        self.previous_response_id = None;
        self.pending_done_response_id = None;
        self.in_flight = false;
    }

    fn mark_closed(&mut self) {
        self.abort_turn();
        self.closed = true;
        self.failed = false;
    }

    fn mark_failed(&mut self) {
        self.abort_turn();
        self.failed = true;
    }

    fn ensure_open(&self) -> Result<(), CompletionError> {
        if self.closed || self.failed {
            return ProviderSnafu {
                msg: "The OpenAI websocket session is closed",
            }
            .fail();
        }

        Ok(())
    }

    fn fail_session(&mut self, error: CompletionError) -> CompletionError {
        self.mark_failed();
        error
    }

    async fn read_next_message(
        &mut self,
    ) -> Result<Option<Result<Message, tungstenite::Error>>, CompletionError> {
        if let Some(timeout_duration) = self.event_timeout {
            match tokio::time::timeout(timeout_duration, self.socket.next()).await {
                Ok(message) => Ok(message),
                Err(_) => Err(self.fail_session(event_timeout_error(timeout_duration))),
            }
        } else {
            Ok(self.socket.next().await)
        }
    }
}

impl<H> Drop for ResponsesWebSocketSession<H> {
    fn drop(&mut self) {
        if !self.closed {
            tracing::warn!(
                target: "rig::completions",
                in_flight = self.in_flight,
                "Dropping an OpenAI websocket session without calling close(); the connection will end without a close handshake"
            );
        }
    }
}

fn terminal_response_result(
    response: CompletionResponse,
) -> Result<CompletionResponse, CompletionError> {
    match response.status {
        ResponseStatus::Completed => Ok(response),
        ResponseStatus::Failed => ProviderSnafu {
            msg: response_error_message(response.error.as_ref(), "failed response"),
        }
        .fail(),
        ResponseStatus::Incomplete => {
            let reason = response
                .incomplete_details
                .as_ref()
                .map(|details| details.reason.as_str())
                .unwrap_or("unknown reason");
            ProviderSnafu {
                msg: format!("OpenAI websocket response was incomplete: {reason}"),
            }
            .fail()
        }
        status => ProviderSnafu {
            msg: format!("OpenAI websocket response ended with status {:?}", status),
        }
        .fail(),
    }
}

fn response_error_message(error: Option<&ResponseError>, fallback: &str) -> String {
    if let Some(error) = error {
        if error.code.is_empty() {
            error.message.clone()
        } else {
            format!("{}: {}", error.code, error.message)
        }
    } else {
        format!("OpenAI websocket returned a {fallback}")
    }
}

fn is_known_streaming_event(kind: &str) -> bool {
    matches!(
        kind,
        "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.failed"
            | "response.incomplete"
            | "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.delta"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
    )
}

/// Returns whether a streaming event can be safely skipped when its payload shape drifts.
///
/// These events are informational deltas or auxiliary completion markers. The websocket
/// session can still complete a turn because the canonical state is later reconstructed
/// from `response.output_item.done` and terminal response events.
fn is_skippable_streaming_event(kind: &str) -> bool {
    matches!(
        kind,
        "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
    )
}

/// Parses one websocket server event into the normalized session event model.
///
/// When OpenAI introduces a small payload drift for auxiliary delta events, we skip those
/// specific events instead of aborting the whole websocket turn. Critical lifecycle and
/// output-item events still fail fast so the session state machine remains trustworthy.
fn parse_server_event(payload: &str) -> Result<Option<ResponsesWebSocketEvent>, CompletionError> {
    #[derive(Deserialize)]
    struct EventType {
        #[serde(rename = "type")]
        kind: String,
    }

    let event_type = serde_json::from_str::<EventType>(payload).context(SerializeSnafu {
        stage: "deserialize-event-type",
    })?;
    match event_type.kind.as_str() {
        "error" => serde_json::from_str(payload)
            .map(|e| Some(ResponsesWebSocketEvent::Error(e)))
            .with_whatever_context(|_| "error"),
        "response.done" => serde_json::from_str(payload)
            .map(|d| Some(ResponsesWebSocketEvent::Done(d)))
            .with_whatever_context(|_| "response.done"),
        kind if is_known_streaming_event(kind) => {
            let chunk = match serde_json::from_str(payload).context(SerializeSnafu {
                stage: "deserialize-payload",
            }) {
                Ok(chunk) => chunk,
                Err(error) if is_skippable_streaming_event(kind) => {
                    tracing::trace!(
                        target: "rig::completions",
                        event_type = kind,
                        payload,
                        error = %error,
                        "Skipping OpenAI websocket streaming event with unsupported payload shape"
                    );
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };

            match chunk {
                StreamingCompletionChunk::Response(response) => {
                    Ok(Some(ResponsesWebSocketEvent::Response(response)))
                }
                StreamingCompletionChunk::Delta(item) => {
                    Ok(Some(ResponsesWebSocketEvent::Item(item)))
                }
            }
        }
        _ => {
            tracing::debug!(
                target: "rig::completions",
                event_type = event_type.kind.as_str(),
                "Skipping unrecognised OpenAI websocket event"
            );
            Ok(None)
        }
    }
}

fn websocket_message_to_text(message: Message) -> Result<Option<String>, CompletionError> {
    match message {
        Message::Text(text) => Ok(Some(text.to_string())),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec())
            .map(Some)
            .map_err(|error| CompletionError::Response {
                msg: error.to_string(),
            }),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(None),
        Message::Close(frame) => {
            let reason = frame
                .map(|frame| frame.reason.to_string())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "without a close reason".to_string());

            ProviderSnafu {
                msg: format!("The OpenAI websocket connection closed {reason}"),
            }
            .fail()
        }
    }
}

fn websocket_url(base_url: &str) -> Result<String, CompletionError> {
    let mut url = Url::parse(base_url).context(UrlSnafu {
        stage: "parse-websocket-url",
    })?;
    match url.scheme() {
        "https" => {
            url.set_scheme("wss")
                .map_err(|_| CompletionError::Provider {
                    msg: "Failed to convert https URL to wss".to_string(),
                })?;
        }
        "http" => {
            url.set_scheme("ws")
                .map_err(|_| CompletionError::Provider {
                    msg: "Failed to convert http URL to ws".to_string(),
                })?;
        }
        scheme => {
            return Err(CompletionError::Provider {
                msg: format!("Unsupported base URL scheme for OpenAI websocket mode: {scheme}"),
            });
        }
    }

    let path = format!("{}/responses", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url.to_string())
}

fn websocket_request(
    url: &str,
    headers: &http::HeaderMap,
) -> Result<http::Request<()>, CompletionError> {
    let mut request = url
        .into_client_request()
        .map_err(|error| CompletionError::Provider {
            msg: format!("Failed to build OpenAI websocket request: {error}"),
        })?;

    for (name, value) in headers {
        request.headers_mut().insert(name, value.clone());
    }

    Ok(request)
}

async fn connect_websocket(
    request: http::Request<()>,
    connect_timeout: Option<Duration>,
) -> Result<OpenAIWebSocket, CompletionError> {
    if let Some(timeout_duration) = connect_timeout {
        match tokio::time::timeout(timeout_duration, connect_async(request)).await {
            Ok(result) => result
                .map(|(socket, _)| socket)
                .map_err(websocket_provider_error),
            Err(_) => Err(connect_timeout_error(timeout_duration)),
        }
    } else {
        connect_async(request)
            .await
            .map(|(socket, _)| socket)
            .map_err(websocket_provider_error)
    }
}

#[inline(always)]
fn connect_timeout_error(timeout: Duration) -> CompletionError {
    ProviderSnafu {
        msg: format!("Timed out connecting to the OpenAI websocket after {timeout:?}"),
    }
    .build()
}

#[inline(always)]
fn event_timeout_error(timeout: Duration) -> CompletionError {
    ProviderSnafu {
        msg: format!("Timed out waiting for the next OpenAI websocket event after {timeout:?}"),
    }
    .build()
}

#[inline(always)]
fn websocket_provider_error(error: tungstenite::Error) -> CompletionError {
    ProviderSnafu {
        msg: error.to_string(),
    }
    .build()
}

#[cfg(test)]
mod tests {
    use super::parse_server_event;

    /// Ensures payload drift on auxiliary tool-call argument deltas does not abort the turn.
    #[test]
    fn malformed_skippable_streaming_event_is_ignored() {
        let event = parse_server_event(
            r#"{
                "type":"response.reasoning_summary_text.delta",
                "output_index":0,
                "sequence_number":1,
                "summary_index":0,
                "delta":{"unexpected":true}
            }"#,
        )
        .expect("skippable events should not fail parsing");

        assert!(event.is_none());
    }

    /// Ensures critical lifecycle events still fail fast when their payload is malformed.
    #[test]
    fn malformed_response_completed_event_still_errors() {
        let error = parse_server_event(
            r#"{
                "type":"response.completed",
                "sequence_number":1
            }"#,
        )
        .expect_err("critical response events must keep failing");

        let message = error.to_string();
        assert!(
            message.contains("deserialize-payload"),
            "expected deserialize-payload context, got {message}"
        );
    }
}
