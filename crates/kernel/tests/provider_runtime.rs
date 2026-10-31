use futures::StreamExt;
use kernel::{ModelFactory, ProviderModelFactory};
use protocol::{ContentBlock, MessageContent, ModelStreamEvent, StopReason};
use serde_json::json;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[path = "support/provider_server.rs"]
mod provider_server;
use provider_server::Server;

static PROVIDER_STREAM_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// Shared in-memory writer used to inspect Provider lifecycle logs.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Returns all UTF-8 tracing output written by the subscriber.
    fn content(&self) -> String {
        String::from_utf8(self.0.lock().expect("log lock").clone())
            .expect("UTF-8 logs")
    }
}

/// One writer handle backed by the shared test buffer.
struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogWriter {
    /// Appends one formatted tracing buffer to the shared capture.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
            .write(buffer)
    }

    /// The in-memory capture has no buffered state to flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedLogWriter;

    /// Creates a writer handle sharing the captured byte buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        CapturedLogWriter(Arc::clone(&self.0))
    }
}

/// Limits and normalized token usage survive the real upstream stream boundary.
#[tokio::test]
async fn provider_final_preserves_usage_and_length_stop() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let body = "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"audit\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":30,\"completion_tokens\":10,\"total_tokens\":40,\"prompt_tokens_details\":{\"cached_tokens\":5}}}\n\ndata: [DONE]\n\n";
    let server = Server::start(vec![(200, body.into())]).await;
    let model = server.factory(false).create().unwrap();
    let events = model
        .stream(Server::request("hi"), CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let final_ = events
        .into_iter()
        .find_map(|event| match event.unwrap() {
            ModelStreamEvent::Finished(result) => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(final_.stop_reason, StopReason::MaxTokens);
    assert_eq!(final_.raw_stop_reason.as_deref(), Some("length"));
    assert_eq!(final_.usage.as_ref().input_tokens, Some(30));
    assert_eq!(final_.usage.as_ref().cached_input_tokens, Some(5));
    assert_eq!(final_.usage.as_ref().total_tokens, Some(40));
}

/// Generation options and compaction metadata reach the actual serialized request.
#[tokio::test]
async fn model_options_and_compaction_summary_reach_provider() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let server = Server::start(vec![(200, Server::completion())]).await;
    let model = server.factory(false).create().unwrap();
    let mut request = Server::request("hi");
    request.options = protocol::ModelRequestOptions {
        temperature: Some(0.25),
        max_tokens: Some(32),
    };
    request.messages[0].content = MessageContent::CompactionSummary {
        compaction: protocol::CompactionSummaryMessage::builder()
            .entry_id(protocol::EntryId::try_from("entry").unwrap())
            .summary("checkpoint body".into())
            .tokens_before(42000)
            .build(),
    };
    for event in model
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
    {
        event.unwrap();
    }
    let requests = server.requests();
    assert_eq!(requests[0]["temperature"], 0.25);
    assert_eq!(requests[0]["max_tokens"], 32);
    assert!(requests[0]["messages"].to_string().contains("42000 tokens"));
    assert!(
        requests[0]["messages"]
            .to_string()
            .contains("checkpoint body")
    );
}

/// EOF without a provider terminal cannot be reported as successful completion.
#[tokio::test]
async fn stream_eof_without_final_is_a_protocol_failure() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let server=Server::start(vec![(200,"data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"audit\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n".into())]).await;
    let model = server.factory(false).create().unwrap();
    let events = model
        .stream(Server::request("hi"), CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().any(Result::is_err));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(ModelStreamEvent::Finished(_))))
    );
}

/// Only Kernel owns retries; one failed provider stream sends exactly one HTTP request.
#[tokio::test]
async fn provider_boundary_does_not_nest_request_retries() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let server = Server::start(vec![(429, "rate limit".into())]).await;
    let model = server.factory(false).create().unwrap();
    let events = model
        .stream(Server::request("hi"), CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().any(Result::is_err));
    assert_eq!(server.requests().len(), 1);
}

/// Supported image results remain available through upstream's user-image fallback.
#[tokio::test]
async fn tool_result_image_is_preserved_in_provider_request() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let server = Server::start(vec![(200, Server::completion())]).await;
    let model = server.factory(false).create().unwrap();
    let request = Server::tool_request(
        vec![
            ContentBlock::Text {
                text: "image follows".into(),
            },
            ContentBlock::Image {
                data: "aW1hZ2U=".into(),
                mime_type: "image/png".into(),
            },
        ],
        false,
    );
    for event in model
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
    {
        event.unwrap();
    }
    assert!(
        server.requests()[0]["messages"]
            .to_string()
            .contains("data:image/png;base64,aW1hZ2U=")
    );
}

/// MCP resource text and metadata stay visible while binary bodies never become model text.
#[tokio::test]
async fn mcp_tool_result_content_has_stable_provider_projection() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let server = Server::start(vec![(200, Server::completion())]).await;
    let model = server.factory(false).create().unwrap();
    let request = Server::tool_request(
        vec![
            ContentBlock::Audio {
                data: "private-audio".into(),
                mime_type: "audio/wav".into(),
            },
            ContentBlock::EmbeddedResource {
                uri: "file:///readme".into(),
                mime_type: Some("text/markdown".into()),
                content: protocol::EmbeddedResourceContent::Text {
                    text: "# Readme".into(),
                },
            },
            ContentBlock::EmbeddedResource {
                uri: "file:///binary".into(),
                mime_type: None,
                content: protocol::EmbeddedResourceContent::Blob {
                    data: "private-binary".into(),
                },
            },
            ContentBlock::Structured {
                value: json!({"count":2}),
            },
        ],
        false,
    );
    for event in model
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
    {
        event.unwrap();
    }
    let payload = server.requests()[0]["messages"].to_string();
    assert!(payload.contains("# Readme") && payload.contains("count"));
    assert!(
        !payload.contains("private-audio")
            && !payload.contains("private-binary")
    );
}

/// Empty tool results preserve correlation and failures retain an explicit visible explanation.
#[tokio::test]
async fn empty_mcp_result_reaches_provider() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    for is_error in [false, true] {
        let server = Server::start(vec![(200, Server::completion())]).await;
        let model = server.factory(false).create().unwrap();
        for event in model
            .stream(
                Server::tool_request(Vec::new(), is_error),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
        {
            event.unwrap();
        }
        let requests = server.requests();
        let message = requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert_eq!(message["tool_call_id"], "call-1");
        if is_error {
            assert!(
                message["content"].as_str().unwrap().contains("Tool failed")
            );
        } else {
            assert_eq!(message["content"], "(no tool output)");
        }
    }
}

/// Shell-history exclusion remains a project policy before the upstream history adapter.
#[tokio::test]
async fn user_bash_context_projection_matches_pi_prefix_semantics() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let server = Server::start(vec![(200, Server::completion())]).await;
    let model = server.factory(false).create().unwrap();
    let mut request = Server::request("visible user input");
    for (output, excluded) in
        [("visible-shell", false), ("private-shell", true)]
    {
        let mut message = request.messages[0].clone();
        message.content = MessageContent::BashExecution {
            bash: protocol::BashExecutionMessage::builder()
                .command("printf sample".into())
                .result(
                    protocol::UserBashResult::builder()
                        .disposition(protocol::UserBashDisposition::Completed)
                        .output(output.into())
                        .exit_code(Some(0))
                        .cancelled(false)
                        .truncated(false)
                        .build(),
                )
                .exclude_from_context(excluded)
                .build(),
        };
        request.messages.push(message);
    }
    for event in model
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
    {
        event.unwrap();
    }
    let payload = server.requests()[0]["messages"].to_string();
    assert!(payload.contains("visible-shell"));
    assert!(!payload.contains("private-shell"));
}

/// Output budgets include the separators that the upstream Chat wire actually writes.
#[tokio::test]
async fn bounded_output_includes_provider_separators() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    for (text, count) in [("x", 20480), ("", 35000)] {
        let mut result = protocol::ToolResult::builder()
            .tool_call_id(protocol::ToolCallId::try_from("call-1").unwrap())
            .blocks(
                (0..count)
                    .map(|_| ContentBlock::Text { text: text.into() })
                    .collect(),
            )
            .is_error(false)
            .build();
        tools::ToolResultProjection::new(
            &mut result,
            protocol::ToolOutputPolicy::Bounded,
            &CancellationToken::new(),
            directory.path(),
        )
        .apply()
        .unwrap();
        let expected = result.output.as_ref().unwrap().visible_text_bytes;
        let server = Server::start(vec![(200, Server::completion())]).await;
        let model = server.factory(false).create().unwrap();
        for event in model
            .stream(
                Server::tool_request(result.blocks, false),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
        {
            event.unwrap();
        }
        let requests = server.requests();
        let message = requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        let bytes = message["content"].as_str().unwrap().len();
        assert!(bytes <= 20480);
        assert_eq!(bytes, expected);
    }
}

/// Native provider logging retains the trace while omitting request and response text.
#[tokio::test]
async fn provider_stream_lifecycle_is_logged_without_message_content() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let server = Server::start(vec![(200, Server::completion())]).await;
    let model = server.factory(false).create().unwrap();
    tracing_futures::WithSubscriber::with_subscriber(
        async {
            tracing::Instrument::instrument(
                async {
                    for event in model
                        .stream(
                            Server::request("private-prompt-sentinel"),
                            CancellationToken::new(),
                        )
                        .await
                        .unwrap()
                        .collect::<Vec<_>>()
                        .await
                    {
                        event.unwrap();
                    }
                },
                tracing::info_span!(
                    "acp_operation",
                    trace_id = "trace-provider-test"
                ),
            )
            .await;
        },
        tracing::Dispatch::new(subscriber),
    )
    .await;
    let output = logs.content();
    assert!(
        output.contains("started Provider request")
            && output.contains("completed Provider request")
    );
    assert!(output.contains("trace-provider-test"));
    assert!(
        !output.contains("private-prompt-sentinel")
            && !output.contains("synthetic-key")
    );
}

/// Static contributions are shared definitions while every catalog gets independent handles.
#[test]
fn provider_model_factory_builds_static_extension_providers() {
    let config: config::AppConfig = toml::from_str(
        r#"
active_model="openai/audit"
[[providers]]
id="openai"
display_name="audit"
provider_type="openai-completions"
base_url="https://provider.invalid"
api_key="synthetic-key"
[[providers.models]]
id="audit"
context_tokens=32768
max_output_tokens=128
"#,
    )
    .unwrap();
    let registration=protocol::StaticExtensionRegistration::builder().providers(vec![protocol::ExtensionProviderRegistration{
        name:"extra".into(),config:json!({"display_name":"extra","provider_type":"anthropic","base_url":"https://provider.invalid","api_key":"synthetic-key","models":[{"id":"extra-model","context_tokens":32768,"max_output_tokens":128}]})
    }]).build();
    let factory = ProviderModelFactory::from_config(
        config::ConfigHandle::from_config(config),
    );
    let first = factory.create_catalog(&registration).unwrap();
    let second = factory.create_catalog(&registration).unwrap();
    assert_eq!(first.profiles().len(), 2);
    assert!(!Arc::ptr_eq(
        &first.resolve("extra", "extra-model").unwrap(),
        &second.resolve("extra", "extra-model").unwrap()
    ));
}
/// Failed requests log typed status and model identity without copying response bodies.
#[tokio::test]
async fn provider_failure_logs_safe_diagnostics() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let server = Server::start(vec![(
        401,
        json!({"error":{"message":"PRIVATE_ERROR_SENTINEL"}}).to_string(),
    )])
    .await;
    let model = server.factory(false).create().unwrap();
    let events = tracing_futures::WithSubscriber::with_subscriber(
        async {
            model
                .stream(
                    Server::request("PRIVATE_PROMPT_SENTINEL"),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        },
        tracing::Dispatch::new(subscriber),
    )
    .await;
    let failure = events
        .into_iter()
        .find_map(Result::err)
        .expect("request failed");
    assert!(failure.to_string().contains("HTTP 401"));
    let output = logs.content();
    assert!(
        output.contains("WARN")
            && output.contains("openai/audit")
            && output.contains("HTTP 401"),
        "{output}"
    );
    assert!(
        !output.contains("PRIVATE_ERROR_SENTINEL")
            && !output.contains("PRIVATE_PROMPT_SENTINEL")
            && !output.contains("synthetic-key")
    );
}

/// Corrupt stored OAuth data fails locally with a safe category in both diagnostics and logs.
#[tokio::test]
async fn provider_auth_failure_is_classified_and_logged() {
    let _guard = PROVIDER_STREAM_TEST_LOCK.lock().await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("chatgpt.json"),
        json!({"access_token":42,"account_id":"PRIVATE_AUTH_SENTINEL"})
            .to_string(),
    )
    .unwrap();
    let config = serde_json::from_value(json!({
        "active_model":"chatgpt/audit",
        "providers":[{"id":"chatgpt","display_name":"audit","provider_type":"responses","base_url":"https://provider.invalid",
            "auth":{"type":"chatgpt","token_store_dir":directory.path()},
            "models":[{"id":"audit","context_tokens":32768,"max_output_tokens":128}]}]
    })).unwrap();
    let model = ProviderModelFactory::from_config(
        config::ConfigHandle::from_config(config),
    )
    .create()
    .unwrap();
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let result = tracing_futures::WithSubscriber::with_subscriber(
        model.stream(
            Server::request("PRIVATE_PROMPT_SENTINEL"),
            CancellationToken::new(),
        ),
        tracing::Dispatch::new(subscriber),
    )
    .await;
    let error = result.err().expect("invalid auth record").to_string();
    assert!(
        error.contains("chatgpt/audit") && error.contains("JSON"),
        "{error}"
    );
    let output = logs.content();
    assert!(
        output.contains("WARN")
            && output.contains("chatgpt/audit")
            && output.contains("JSON"),
        "{output}"
    );
    assert!(
        !output.contains("PRIVATE_AUTH_SENTINEL")
            && !output.contains("PRIVATE_PROMPT_SENTINEL")
    );
}
