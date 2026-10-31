use kernel::ProviderModelFactory;
use protocol::{
    AgentMessage, ContentBlock, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ModelRequest, TimestampMs, TurnId,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

// Each integration target uses a different subset of this local HTTP fixture.
#[allow(dead_code)]
impl Server {
    /// Creates a real Kernel around this server and an isolated production JSONL store.
    pub fn kernel(
        &self,
        root: &std::path::Path,
        retry: config::RetryConfig,
        extensions: extension::StaticExtensionFactory,
    ) -> kernel::Kernel {
        let clock: Arc<dyn store::Clock> = Arc::new(store::SystemClock);
        kernel::KernelFactory::builder()
            .model_factory(Arc::new(self.factory(true)))
            .retry_policy(retry.model_policy())
            .provider_retry_delay_limit_ms(retry.provider.max_retry_delay_ms)
            .tool_factory(Arc::new(tools::BuiltinToolFactory::new()))
            .store_factory(Arc::new(store::JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(extensions))
            .prompt_factory(Arc::new(prompt::FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .clock(clock)
            .id_generator(Arc::new(kernel::NanoidIdGenerator))
            .build()
            .build()
            .expect("valid synthetic test state")
    }

    /// Returns only the synthetic JSON requests captured by the local server.
    pub fn requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .expect("valid synthetic test state")
            .clone()
    }

    /// Produces a complete Chat stream for tests that focus on outgoing request conversion.
    pub fn completion() -> String {
        "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"audit\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into()
    }

    /// Creates a correlated tool call and result so upstream history never treats the result as orphaned.
    pub fn tool_request(
        blocks: Vec<ContentBlock>,
        is_error: bool,
    ) -> ModelRequest {
        let mut request = Self::request("initial");
        let mut assistant =
            request.messages.first().expect("user message").clone();
        assistant.identity.message_id = MessageId::try_from("assistant")
            .expect("valid synthetic test state");
        assistant.content = MessageContent::Assistant {
            blocks: vec![ContentBlock::ToolCall {
                tool_call_id: protocol::ToolCallId::try_from("call-1")
                    .expect("valid synthetic test state"),
                name: "lookup".into(),
                arguments: serde_json::json!({}),
                invalid_arguments: None,
            }],
            metadata: protocol::AssistantMetadata::builder()
                .provider_id("openai".into())
                .model_id("audit".into())
                .stop_reason(protocol::StopReason::ToolUse)
                .usage(protocol::ModelUsage::default())
                .build(),
            model_message: None,
        };
        let mut result =
            request.messages.first().expect("user message").clone();
        result.identity.message_id =
            MessageId::try_from("result").expect("valid synthetic test state");
        result.content = MessageContent::ToolResult {
            tool_call_id: protocol::ToolCallId::try_from("call-1")
                .expect("valid synthetic test state"),
            blocks,
            is_error,
            details: None,
            output: None,
            cancelled: false,
        };
        request.messages.extend([assistant, result]);
        request.tools.push(protocol::ToolDefinition {
            name: "lookup".into(),
            description: "lookup".into(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        });
        request
    }

    /// Exercises the production HTTP transport against a finite local response sequence.
    pub async fn start(replies: Vec<(u16, String)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("valid synthetic test state");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("valid synthetic test state")
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            for (status, body) in replies {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("valid synthetic test state");
                let mut received = Vec::new();
                let mut chunk = [0; 8192];
                loop {
                    let count = socket
                        .read(&mut chunk)
                        .await
                        .expect("valid synthetic test state");
                    if count == 0 {
                        break;
                    }
                    received.extend_from_slice(
                        chunk.get(..count).expect("read buffer bound"),
                    );
                    if let Some(end) =
                        received.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(
                            received.get(..end).expect("header boundary"),
                        );
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| {
                                        value.trim().parse::<usize>().expect(
                                            "valid synthetic test state",
                                        )
                                    })
                            })
                            .unwrap_or(0);
                        if received.len() >= end + 4 + length {
                            capture
                                .lock()
                                .expect("valid synthetic test state")
                                .push(
                                    serde_json::from_slice(
                                        received
                                            .get(end + 4..end + 4 + length)
                                            .expect("complete HTTP body"),
                                    )
                                    .expect("valid synthetic test state"),
                                );
                            break;
                        }
                    }
                }
                let response = format!(
                    "HTTP/1.1 {status} response\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("valid synthetic test state");
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    /// Builds a production factory without a private fake-model API.
    pub fn factory(&self, responses: bool) -> ProviderModelFactory {
        let protocol = if responses {
            "responses"
        } else {
            "openai-completions"
        };
        ProviderModelFactory::from_config(config::ConfigHandle::from_config(
            self.configuration("openai", protocol),
        ))
    }

    /// Makes protocol-specific test configuration while retaining the same local transport.
    pub fn configuration(&self, id: &str, protocol: &str) -> config::AppConfig {
        let document = format!(
            r#"
active_model = "{id}/audit"
[[providers]]
id = "{id}"
display_name = "audit"
provider_type = "{protocol}"
base_url = "{}"
api_key = "synthetic-key"
[[providers.models]]
id = "audit"
context_tokens = 32768
max_output_tokens = 128
"#,
            self.url
        );
        toml::from_str(&document).expect("synthetic provider configuration")
    }

    /// Creates one complete project request for the upstream protocol boundary.
    pub fn request(text: &str) -> ModelRequest {
        let time = TimestampMs::from(1);
        ModelRequest {
            messages: vec![AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("m")
                        .expect("valid synthetic test state"),
                    turn_id: TurnId::try_from("t")
                        .expect("valid synthetic test state"),
                },
                timing: MessageTiming::try_from((time, time, time))
                    .expect("valid synthetic test state"),
                content: MessageContent::User {
                    blocks: vec![ContentBlock::Text { text: text.into() }],
                },
            }],
            tools: Vec::new(),
            options: Default::default(),
        }
    }
}

impl Drop for Server {
    /// Releases listeners even when a protocol assertion fails.
    fn drop(&mut self) {
        self.task.abort();
    }
}
