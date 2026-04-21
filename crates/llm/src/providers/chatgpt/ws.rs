use std::collections::HashMap;
use std::fmt::Debug;

use futures::StreamExt;
use serde_json;
use tokio::sync::mpsc;

use crate::{
    completion::{CompletionError, ProviderSnafu, message::ReasoningContent},
    http_client::HttpClientExt,
    streaming::{RawStreamingChoice, RawStreamingToolCall, ToolCallDeltaContent},
};

use super::{
    ChatGPTCompletionResponse, Client, ResponsesCompletionModel, ResponsesWebSocketSession,
    responses_api,
};

/// Completion model that executes ChatGPT responses through websocket transport.
#[derive(Clone)]
pub struct WsCompletionModel<H = reqwest::Client> {
    inner: ResponsesCompletionModel<H>,
}

impl<H> WsCompletionModel<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    /// Creates a websocket-backed ChatGPT completion model from an existing responses model.
    pub fn new(inner: ResponsesCompletionModel<H>) -> Self {
        Self { inner }
    }
}

impl<H> crate::completion::CompletionModel for WsCompletionModel<H>
where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    type Response = ChatGPTCompletionResponse;
    type StreamingResponse = responses_api::streaming::StreamingCompletionResponse;
    type Client = Client<H>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self::new(ResponsesCompletionModel::new(client.clone(), model))
    }

    /// Rebuilds one blocking completion by exhausting websocket streaming events.
    async fn completion(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<crate::completion::CompletionResponse<Self::Response>, CompletionError> {
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
            raw_response: ChatGPTCompletionResponse {
                usage,
                message_id: message_id.clone(),
            },
            message_id,
        })
    }

    /// Streams websocket events and maps them into generic raw streaming choices.
    async fn stream(
        &self,
        completion_request: crate::completion::CompletionRequest,
    ) -> Result<
        crate::streaming::StreamingCompletionResponse<Self::StreamingResponse>,
        CompletionError,
    > {
        let mut session = self.inner.websocket_session().await?;
        session.send(completion_request).await?;

        let (tx, rx) = mpsc::channel::<
            Result<
                RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>,
                CompletionError,
            >,
        >(32);

        tokio::spawn(run_websocket_completion_stream(session, tx));

        Ok(crate::streaming::StreamingCompletionResponse::stream(
            Box::pin(futures::stream::unfold(rx, |mut rx| async {
                rx.recv().await.map(|item| (item, rx))
            })),
        ))
    }
}

/// Drains a websocket completion session and emits mapped streaming choices.
async fn run_websocket_completion_stream<H>(
    mut session: ResponsesWebSocketSession<H>,
    tx: mpsc::Sender<
        Result<
            RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>,
            CompletionError,
        >,
    >,
) where
    H: HttpClientExt + Clone + Debug + Default + Send + Sync + 'static,
{
    let mut item_mapper = WebsocketItemMapper::new(tx.clone());
    let mut final_usage = responses_api::ResponsesUsage::default();
    let mut terminal_message_id: Option<String> = None;

    loop {
        let event = match session.next_event().await {
            Ok(event) => event,
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                let _ = session.close().await;
                return;
            }
        };

        let should_continue = match item_mapper
            .handle_websocket_event(event, &mut final_usage, &mut terminal_message_id)
            .await
        {
            Ok(should_continue) => should_continue,
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                let _ = session.close().await;
                return;
            }
        };

        if !should_continue {
            let _ = session.close().await;
            return;
        }
    }
}

/// Maps websocket output-item chunks into stream choices while preserving ID correlation.
struct WebsocketItemMapper {
    tx: mpsc::Sender<
        Result<
            RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>,
            CompletionError,
        >,
    >,
    tool_call_mapping: HashMap<String, String>,
}

impl WebsocketItemMapper {
    /// Creates a mapper with no cached function-call to internal id association.
    fn new(
        tx: mpsc::Sender<
            Result<
                RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>,
                CompletionError,
            >,
        >,
    ) -> Self {
        Self {
            tx,
            tool_call_mapping: HashMap::new(),
        }
    }

    /// Maps one websocket output-item chunk and records correlation metadata when needed.
    fn map_item(
        &mut self,
        item: responses_api::streaming::ItemChunkKind,
        terminal_message_id: &mut Option<String>,
    ) -> Vec<RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>> {
        match item {
            responses_api::streaming::ItemChunkKind::OutputItemAdded(message) => {
                if let responses_api::Output::FunctionCall(function_call) = message.item {
                    let internal_call_id = self.call_id(&function_call.id);
                    vec![RawStreamingChoice::ToolCallDelta {
                        id: function_call.id,
                        internal_call_id,
                        content: ToolCallDeltaContent::Name(function_call.name),
                    }]
                } else {
                    Vec::new()
                }
            }
            responses_api::streaming::ItemChunkKind::OutputItemDone(done) => {
                let mut choices = Vec::new();
                match done.item {
                    responses_api::Output::FunctionCall(function_call) => {
                        let internal_call_id = self.call_id(&function_call.id);
                        choices.push(RawStreamingChoice::ToolCall(
                            RawStreamingToolCall::new(
                                function_call.id,
                                function_call.name,
                                function_call.arguments,
                            )
                            .with_internal_call_id(internal_call_id)
                            .with_call_id(function_call.call_id),
                        ));
                    }
                    responses_api::Output::Message(message) => {
                        if terminal_message_id.is_none() {
                            *terminal_message_id = Some(message.id.clone());
                            choices.push(RawStreamingChoice::MessageId(message.id));
                        }
                    }
                    responses_api::Output::Reasoning {
                        id,
                        summary,
                        encrypted_content,
                        ..
                    } => {
                        choices.extend(summary.into_iter().map(|summary| {
                            RawStreamingChoice::Reasoning {
                                id: Some(id.clone()),
                                content: match summary {
                                    responses_api::ReasoningSummary::SummaryText { text } => {
                                        ReasoningContent::Summary(text)
                                    }
                                },
                            }
                        }));
                        if let Some(encrypted_content) = encrypted_content {
                            choices.push(RawStreamingChoice::Reasoning {
                                id: Some(id),
                                content: ReasoningContent::Encrypted(encrypted_content),
                            });
                        }
                    }
                }
                choices
            }
            responses_api::streaming::ItemChunkKind::OutputTextDelta(delta) => {
                vec![RawStreamingChoice::Message(delta.delta)]
            }
            responses_api::streaming::ItemChunkKind::RefusalDelta(delta) => {
                vec![RawStreamingChoice::Message(delta.delta)]
            }
            responses_api::streaming::ItemChunkKind::FunctionCallArgsDelta(delta) => {
                let internal_call_id = self.call_id(&delta.item_id);
                vec![RawStreamingChoice::ToolCallDelta {
                    id: delta.item_id,
                    internal_call_id,
                    content: ToolCallDeltaContent::Delta(delta.delta),
                }]
            }
            responses_api::streaming::ItemChunkKind::ReasoningSummaryTextDelta(delta) => {
                vec![RawStreamingChoice::ReasoningDelta {
                    id: None,
                    reasoning: delta.delta,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Converts terminal response chunks to stream choices and tracks terminal message id.
    fn terminal_output_choices(
        &mut self,
        response: &responses_api::CompletionResponse,
        usage: &mut responses_api::ResponsesUsage,
        terminal_message_id: &mut Option<String>,
    ) -> Vec<RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>> {
        *usage = response.usage.clone().unwrap_or_default();

        let mut choices = Vec::new();

        if terminal_message_id.is_none() {
            let message_id = response.output.iter().find_map(|item| match item {
                responses_api::Output::Message(message) => Some(message.id.clone()),
                _ => None,
            });

            if let Some(message_id) = message_id {
                *terminal_message_id = Some(message_id.clone());
                choices.push(RawStreamingChoice::MessageId(message_id));
            }
        }

        choices
    }

    /// Returns a stable internal tool-call identifier for a websocket function/item id.
    fn call_id(&mut self, function_call_id: &str) -> String {
        self.tool_call_mapping
            .entry(function_call_id.to_string())
            .or_insert_with(|| function_call_id.to_string())
            .clone()
    }

    /// Handles one websocket event and returns whether the stream loop should keep running.
    async fn handle_websocket_event(
        &mut self,
        event: responses_api::websocket::ResponsesWebSocketEvent,
        final_usage: &mut responses_api::ResponsesUsage,
        terminal_message_id: &mut Option<String>,
    ) -> Result<bool, CompletionError> {
        match event {
            responses_api::websocket::ResponsesWebSocketEvent::Item(item) => {
                let choices = self.map_item(item.data, terminal_message_id);
                self.emit_choices(choices).await?;

                Ok(true)
            }
            responses_api::websocket::ResponsesWebSocketEvent::Response(response) => {
                if !matches!(
                    response.kind,
                    responses_api::streaming::ResponseChunkKind::ResponseCompleted
                        | responses_api::streaming::ResponseChunkKind::ResponseFailed
                        | responses_api::streaming::ResponseChunkKind::ResponseIncomplete
                ) {
                    return Ok(true);
                }

                let response = super::validate_terminal_response(response.response)?;

                let mut choices =
                    self.terminal_output_choices(&response, final_usage, terminal_message_id);

                choices.push(RawStreamingChoice::FinalResponse(
                    responses_api::streaming::StreamingCompletionResponse {
                        usage: final_usage.clone(),
                    },
                ));

                self.emit_choices(choices).await?;

                Ok(false)
            }
            responses_api::websocket::ResponsesWebSocketEvent::Done(done) => {
                let response = match serde_json::from_value(done.response) {
                    Ok(response) => response,
                    Err(error) => {
                        return Err(CompletionError::Provider {
                            msg: format!("OpenAI websocket done response parse error: {error}"),
                        });
                    }
                };

                let response = super::validate_terminal_response(response)?;
                let mut choices =
                    self.terminal_output_choices(&response, final_usage, terminal_message_id);

                choices.push(RawStreamingChoice::FinalResponse(
                    responses_api::streaming::StreamingCompletionResponse {
                        usage: final_usage.clone(),
                    },
                ));

                self.emit_choices(choices).await?;

                Ok(false)
            }
            responses_api::websocket::ResponsesWebSocketEvent::Error(error) => ProviderSnafu {
                msg: error.to_string(),
            }
            .fail(),
        }
    }

    /// Emits mapped choices to websocket stream and wraps sender failure as provider errors.
    async fn emit_choices(
        &self,
        choices: Vec<RawStreamingChoice<responses_api::streaming::StreamingCompletionResponse>>,
    ) -> Result<(), CompletionError> {
        for choice in choices {
            self.tx
                .send(Ok(choice))
                .await
                .map_err(|_| CompletionError::Provider {
                    msg: "Websocket streaming consumer closed before all choices were emitted"
                        .to_string(),
                })?;
        }

        Ok(())
    }
}
