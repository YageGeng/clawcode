use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snafu::Snafu;

use crate::{
    http_client, json_utils,
    one_or_many::OneOrMany,
    streaming::StreamingCompletionResponse,
    usage::{GetTokenUsage, Usage},
};

use super::message::{AssistantContent, Message, ToolChoice};

pub trait CompletionModel: Clone {
    /// The raw response type returned by the underlying completion model.
    type Response: DeserializeOwned;
    /// The raw response type returned by the underlying completion model when streaming.
    type StreamingResponse: Clone + Unpin + Serialize + DeserializeOwned + GetTokenUsage;

    type Client;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self;

    /// Generates a completion response for the given completion request.
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse<Self::Response>, CompletionError>>;

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<
        Output = Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>,
    >;

    /// Generates a completion request builder for the given `prompt`.
    fn completion_request(&self, prompt: impl Into<Message>) -> CompletionRequestBuilder<Self> {
        CompletionRequestBuilder::new(self.clone(), prompt)
    }
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Optional model override for this request.
    pub model: Option<String>,
    /// Legacy preamble field preserved for backwards compatibility.
    ///
    /// New code should prefer a leading [`Message::System`]
    /// in `chat_history` as the canonical representation of system instructions.
    pub preamble: Option<String>,
    /// The chat history to be sent to the completion model provider.
    /// The very last message will always be the prompt (hence why there is *always* one)
    pub chat_history: OneOrMany<Message>,
    // /// The tools to be sent to the completion model provider
    // pub tools: Vec<ToolDefinition>,
    /// The temperature to be sent to the completion model provider
    pub temperature: Option<f64>,
    /// The max tokens to be sent to the completion model provider
    pub max_tokens: Option<u64>,
    /// The tools to be sent to the completion model provider
    pub tools: Vec<ToolDefinition>,
    /// Whether tools are required to be used by the model provider or not before providing a response.
    pub tool_choice: Option<ToolChoice>,
    // /// Whether tools are required to be used by the model provider or not before providing a response.
    // pub tool_choice: Option<ToolChoice>,
    /// Additional provider-specific parameters to be sent to the completion model provider
    pub additional_params: Option<serde_json::Value>,
    /// Optional JSON Schema for structured output. When set, providers that support
    /// native structured outputs will constrain the model's response to match this schema.
    pub output_schema: Option<schemars::Schema>,
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum CompletionError {
    #[snafu(display("Serialization error: {stage}: {source}"))]
    Serialize {
        source: serde_json::Error,
        stage: String,
    },
    #[snafu(display("Client error: {stage}: {source}"))]
    Client {
        source: http_client::Error,
        stage: String,
    },
    #[snafu(display("Provider error: {msg}"))]
    Provider { msg: String },
    #[snafu(display("Response error: {msg}"))]
    Response { msg: String },
    #[snafu(whatever, display("Request error: {source:?}, message: {message}"))]
    Request {
        message: String,
        // Having a `source` is optional, but if it is present, it must
        // have this specific attribute and type:
        #[snafu(source(from(Box<dyn std::error::Error + 'static+Send+Sync>, Some)))]
        source: Option<Box<dyn std::error::Error + 'static + Send + Sync>>,
    },
}

#[derive(Debug)]
pub struct CompletionResponse<T> {
    /// The completion choice (represented by one or more assistant message content)
    /// returned by the completion model provider
    pub choice: OneOrMany<AssistantContent>,
    /// Tokens used during prompting and responding
    pub usage: Usage,
    /// The raw response returned by the completion model provider
    pub raw_response: T,
    /// Provider-assigned message ID (e.g. OpenAI Responses API `msg_` ID).
    /// Used to pair reasoning input items with their output items in multi-turn.
    pub message_id: Option<String>,
}

pub struct CompletionRequestBuilder<M: CompletionModel> {
    model: M,
    prompt: Message,
    request_model: Option<String>,
    preamble: Option<String>,
    chat_history: Vec<Message>,
    tools: Vec<ToolDefinition>,
    provider_tools: Vec<ProviderToolDefinition>,
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    tool_choice: Option<ToolChoice>,
    additional_params: Option<serde_json::Value>,
    output_schema: Option<schemars::Schema>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Provider-native tool definition.
///
/// Stored under `additional_params.tools` and forwarded by providers that support
/// provider-managed tools.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ProviderToolDefinition {
    /// Tool type/kind name as expected by the target provider (for example `web_search`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Additional provider-specific configuration for this hosted tool.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub config: serde_json::Map<String, serde_json::Value>,
}

impl ProviderToolDefinition {
    /// Creates a provider-hosted tool definition by type.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            config: serde_json::Map::new(),
        }
    }

    /// Adds a provider-specific configuration key/value.
    pub fn with_config(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.config.insert(key.into(), value);
        self
    }
}

impl<M: CompletionModel> CompletionRequestBuilder<M> {
    pub fn new(model: M, prompt: impl Into<Message>) -> Self {
        Self {
            model,
            prompt: prompt.into(),
            request_model: None,
            preamble: None,
            chat_history: Vec::new(),
            tools: Vec::new(),
            provider_tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        }
    }

    /// Sets the preamble for the completion request.
    pub fn preamble(mut self, preamble: String) -> Self {
        // Legacy public API: funnel preamble into canonical system messages at build-time.
        self.preamble = Some(preamble);
        self
    }

    /// Overrides the model used for this request.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.request_model = Some(model.into());
        self
    }

    /// Overrides the model used for this request.
    pub fn model_opt(mut self, model: Option<String>) -> Self {
        self.request_model = model;
        self
    }

    pub fn without_preamble(mut self) -> Self {
        self.preamble = None;
        self
    }

    /// Adds a message to the chat history for the completion request.
    pub fn message(mut self, message: Message) -> Self {
        self.chat_history.push(message);
        self
    }

    /// Adds a list of messages to the chat history for the completion request.
    pub fn messages(self, messages: Vec<Message>) -> Self {
        messages
            .into_iter()
            .fold(self, |builder, msg| builder.message(msg))
    }

    /// Adds a tool to the completion request.
    pub fn tool(mut self, tool: ToolDefinition) -> Self {
        self.tools.push(tool);
        self
    }

    /// Adds a list of tools to the completion request.
    pub fn tools(self, tools: Vec<ToolDefinition>) -> Self {
        tools
            .into_iter()
            .fold(self, |builder, tool| builder.tool(tool))
    }

    /// Adds a provider-hosted tool to the completion request.
    pub fn provider_tool(mut self, tool: ProviderToolDefinition) -> Self {
        self.provider_tools.push(tool);
        self
    }

    /// Adds provider-hosted tools to the completion request.
    pub fn provider_tools(self, tools: Vec<ProviderToolDefinition>) -> Self {
        tools
            .into_iter()
            .fold(self, |builder, tool| builder.provider_tool(tool))
    }

    /// Adds additional parameters to the completion request.
    /// This can be used to set additional provider-specific parameters. For example,
    /// Cohere's completion models accept a `connectors` parameter that can be used to
    /// specify the data connectors used by Cohere when executing the completion
    /// (see `examples/cohere_connectors.rs`).
    pub fn additional_params(mut self, additional_params: serde_json::Value) -> Self {
        match self.additional_params {
            Some(params) => {
                self.additional_params = Some(json_utils::merge(params, additional_params));
            }
            None => {
                self.additional_params = Some(additional_params);
            }
        }
        self
    }

    /// Sets the additional parameters for the completion request.
    /// This can be used to set additional provider-specific parameters. For example,
    /// Cohere's completion models accept a `connectors` parameter that can be used to
    /// specify the data connectors used by Cohere when executing the completion
    /// (see `examples/cohere_connectors.rs`).
    pub fn additional_params_opt(mut self, additional_params: Option<serde_json::Value>) -> Self {
        self.additional_params = additional_params;
        self
    }

    /// Sets the temperature for the completion request.
    pub fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the temperature for the completion request.
    pub fn temperature_opt(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    /// Sets the max tokens for the completion request.
    /// Note: This is required if using Anthropic
    pub fn max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Sets the max tokens for the completion request.
    /// Note: This is required if using Anthropic
    pub fn max_tokens_opt(mut self, max_tokens: Option<u64>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Sets the thing.
    pub fn tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    /// Sets the output schema for structured output. When set, providers that support
    /// native structured outputs will constrain the model's response to match this schema.
    /// NOTE: For direct type conversion, you may want to use `Agent::prompt_typed()` - using this method
    /// with `Agent::prompt()` will still output a String at the end, it'll just be compatible with whatever
    /// type you want to use here. This method is primarily an escape hatch for agents being used as tools
    /// to still be able to leverage structured outputs.
    pub fn output_schema(mut self, schema: schemars::Schema) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Sets the output schema for structured output from an optional value.
    /// NOTE: For direct type conversion, you may want to use `Agent::prompt_typed()` - using this method
    /// with `Agent::prompt()` will still output a String at the end, it'll just be compatible with whatever
    /// type you want to use here. This method is primarily an escape hatch for agents being used as tools
    /// to still be able to leverage structured outputs.
    pub fn output_schema_opt(mut self, schema: Option<schemars::Schema>) -> Self {
        self.output_schema = schema;
        self
    }

    /// Builds the completion request.
    pub fn build(self) -> CompletionRequest {
        let mut chat_history = self.chat_history;
        if let Some(preamble) = self.preamble {
            chat_history.insert(0, Message::system(preamble));
        }
        let chat_history = OneOrMany::many([chat_history, vec![self.prompt]].concat())
            .expect("There will always be atleast the prompt");
        let additional_params = merge_provider_tools_into_additional_params(
            self.additional_params,
            self.provider_tools,
        );

        CompletionRequest {
            model: self.request_model,
            preamble: None,
            chat_history,
            tools: self.tools,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            tool_choice: self.tool_choice,
            additional_params,
            output_schema: self.output_schema,
        }
    }

    /// Sends the completion request to the completion model provider and returns the completion response.
    pub async fn send(self) -> Result<CompletionResponse<M::Response>, CompletionError> {
        let model = self.model.clone();
        model.completion(self.build()).await
    }

    /// Stream the completion request
    pub async fn stream<'a>(
        self,
    ) -> Result<StreamingCompletionResponse<M::StreamingResponse>, CompletionError>
    where
        <M as CompletionModel>::StreamingResponse: 'a,
        Self: 'a,
    {
        let model = self.model.clone();
        model.stream(self.build()).await
    }
}

fn merge_provider_tools_into_additional_params(
    additional_params: Option<serde_json::Value>,
    provider_tools: Vec<ProviderToolDefinition>,
) -> Option<serde_json::Value> {
    if provider_tools.is_empty() {
        return additional_params;
    }

    let mut provider_tools_json = provider_tools
        .into_iter()
        .map(|ProviderToolDefinition { kind, mut config }| {
            // Force the provider tool type from the strongly-typed field.
            config.insert("type".to_string(), serde_json::Value::String(kind));
            serde_json::Value::Object(config)
        })
        .collect::<Vec<_>>();

    let mut params_map = match additional_params {
        Some(serde_json::Value::Object(map)) => map,
        Some(serde_json::Value::Bool(stream)) => {
            let mut map = serde_json::Map::new();
            map.insert("stream".to_string(), serde_json::Value::Bool(stream));
            map
        }
        _ => serde_json::Map::new(),
    };

    let mut merged_tools = match params_map.remove("tools") {
        Some(serde_json::Value::Array(existing)) => existing,
        _ => Vec::new(),
    };
    merged_tools.append(&mut provider_tools_json);
    params_map.insert("tools".to_string(), serde_json::Value::Array(merged_tools));
    Some(serde_json::Value::Object(params_map))
}
