use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use kernel::{
    Model, ModelError, ModelFactory, ProviderModel, ProviderModelFactory,
};
use protocol::{
    AgentMessage, BashExecutionMessage, ContentBlock, MessageContent,
    MessageId, MessageIdentity, MessageTiming, ModelProfile, ModelRequest,
    ModelStreamEvent, StaticExtensionRegistration, StopReason, TimestampMs,
    TurnId, UserBashResult,
};
use provider::completion::{CompletionError, CompletionRequest, Usage};
use provider::factory::{
    DynLlmStream, Llm, LlmCompletion, LlmStreamEvent, ProviderFinal,
    ProviderFinishReason,
};
use provider::wasm_compat::WasmBoxedFuture;
use tokio_util::sync::CancellationToken;

/// Scripted dynamic provider that exposes request acquisition counts.
#[derive(Debug)]
struct FixtureLlm {
    acquisitions: Mutex<VecDeque<Result<Vec<LlmStreamEvent>, CompletionError>>>,
    acquisition_count: AtomicUsize,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl FixtureLlm {
    /// Creates a scripted provider from ordered acquisition outcomes.
    fn new(
        acquisitions: Vec<Result<Vec<LlmStreamEvent>, CompletionError>>,
    ) -> Self {
        Self {
            acquisitions: Mutex::new(acquisitions.into()),
            acquisition_count: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Returns how many provider stream acquisitions were attempted.
    fn acquisition_count(&self) -> usize {
        self.acquisition_count.load(Ordering::SeqCst)
    }

    /// Returns the newest provider request captured by this fixture.
    fn last_request(&self) -> Option<CompletionRequest> {
        self.requests
            .lock()
            .expect("fixture request lock")
            .last()
            .cloned()
    }

    /// Returns a successful provider Final used after retry acquisition.
    fn successful_final() -> Vec<LlmStreamEvent> {
        vec![LlmStreamEvent::Final(ProviderFinal {
            finish_reason: ProviderFinishReason::Stop,
            raw_finish_reason: Some("stop".to_string()),
            usage: Some(
                Usage::builder()
                    .input_tokens(4)
                    .output_tokens(1)
                    .total_tokens(5)
                    .cached_input_tokens(0)
                    .cache_creation_input_tokens(0)
                    .build(),
            ),
        })]
    }
}

impl Llm for FixtureLlm {
    /// Returns the deterministic fixture provider identifier.
    fn provider_id(&self) -> &str {
        "fixture"
    }

    /// Returns the deterministic fixture model identifier.
    fn model_id(&self) -> &str {
        "fixture-model"
    }

    /// Rejects the unused non-streaming path in this fixture.
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> WasmBoxedFuture<'_, Result<LlmCompletion, CompletionError>> {
        Box::pin(async {
            Err(CompletionError::ProviderError(
                "fixture completion is unsupported".to_string(),
            ))
        })
    }

    /// Returns the next scripted provider stream acquisition result.
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> WasmBoxedFuture<'_, Result<DynLlmStream, CompletionError>> {
        self.acquisition_count.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("fixture request lock")
            .push(request);
        Box::pin(async {
            let events = self
                .acquisitions
                .lock()
                .expect("fixture acquisition lock")
                .pop_front()
                .expect("scripted acquisition");
            events.map(|events| {
                Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
                    as DynLlmStream
            })
        })
    }
}

/// Builds a complete user request accepted by the provider conversion boundary.
fn sample_request() -> ModelRequest {
    let timestamp = TimestampMs::from(100);
    ModelRequest {
        messages: vec![AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("message-1")
                    .expect("message id"),
                turn_id: TurnId::try_from("turn-1").expect("turn id"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("message timing"),
            content: MessageContent::User {
                blocks: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            },
        }],
        tools: Vec::new(),
    }
}

/// Builds the stable model profile used by provider adapter tests.
fn sample_profile() -> ModelProfile {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("fixture-model".to_string())
        .display_name("Fixture Model".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
}

/// Provider Final preserves a length stop and complete usage accounting.
#[tokio::test]
async fn provider_final_preserves_usage_and_length_stop() {
    let llm = Arc::new(FixtureLlm::new(vec![Ok(vec![LlmStreamEvent::Final(
        ProviderFinal {
            finish_reason: ProviderFinishReason::Length,
            raw_finish_reason: Some("length".to_string()),
            usage: Some(
                Usage::builder()
                    .input_tokens(90)
                    .output_tokens(10)
                    .total_tokens(100)
                    .cached_input_tokens(3)
                    .cache_creation_input_tokens(2)
                    .build(),
            ),
        },
    )])]));
    let model = ProviderModel::new(
        sample_profile(),
        Arc::clone(&llm) as Arc<dyn Llm>,
        config::ProviderRetryConfig::default(),
    );
    let mut stream = model
        .stream(sample_request(), CancellationToken::new())
        .await
        .expect("provider stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("model event"));
    }

    assert!(
        matches!(events.last(), Some(ModelStreamEvent::Finished(final_))
        if final_.stop_reason == StopReason::MaxTokens
            && final_.usage.input_tokens == 90
            && final_.usage.cache_read_tokens == 3
            && final_.usage.cache_write_tokens == 2
            && final_.usage.total_tokens == 100)
    );
}

/// Provider EOF without an explicit Final becomes a stream protocol failure.
#[tokio::test]
async fn stream_eof_without_final_is_a_protocol_failure() {
    let llm = Arc::new(FixtureLlm::new(vec![Ok(Vec::new())]));
    let model = ProviderModel::new(
        sample_profile(),
        llm as Arc<dyn Llm>,
        config::ProviderRetryConfig::default(),
    );
    let mut stream = model
        .stream(sample_request(), CancellationToken::new())
        .await
        .expect("provider stream acquisition");

    assert!(matches!(
        stream.next().await,
        Some(Err(ModelError::Stream(_)))
    ));
}

/// Preflight validates the constructed provider without issuing a paid request.
#[tokio::test]
async fn preflight_does_not_acquire_a_provider_stream() {
    let llm = Arc::new(FixtureLlm::new(Vec::new()));
    let model = ProviderModel::new(
        sample_profile(),
        Arc::clone(&llm) as Arc<dyn Llm>,
        config::ProviderRetryConfig::default(),
    );

    model.preflight().await.expect("preflight");
    assert_eq!(llm.acquisition_count(), 0);
}

/// A retryable 503 reacquires the provider stream once after backoff.
#[tokio::test(start_paused = true)]
async fn retryable_provider_failure_reacquires_the_stream() {
    let llm = Arc::new(FixtureLlm::new(vec![
        Err(CompletionError::HttpError(
            provider::http_client::Error::InvalidStatusCode(
                http::StatusCode::SERVICE_UNAVAILABLE,
            ),
        )),
        Ok(FixtureLlm::successful_final()),
    ]));
    let model = ProviderModel::new(
        sample_profile(),
        Arc::clone(&llm) as Arc<dyn Llm>,
        config::ProviderRetryConfig {
            timeout_ms: None,
            max_retries: Some(1),
            max_retry_delay_ms: 60_000,
        },
    );
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(async move {
        model.stream(sample_request(), cancellation).await
    });

    tokio::task::yield_now().await;
    assert_eq!(llm.acquisition_count(), 1);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let result = task.await.expect("retry task");
    let _stream = result.expect("retry should acquire a stream");
    assert_eq!(llm.acquisition_count(), 2);
}

/// Static provider declarations participate in the immutable model catalog.
#[test]
fn provider_model_factory_builds_static_extension_providers() {
    let base_provider = config::LlmProvider {
        id: config::ProviderId::Other("base".to_string()),
        display_name: "Base".to_string(),
        provider_type: config::ProviderType::OpenaiCompletions,
        base_url: "https://example.com/v1".to_string(),
        api_key: Some(config::ApiKeyConfig::Plaintext("sk-test".to_string())),
        auth: None,
        models: vec![config::LlmModel {
            id: "base-model".to_string(),
            display_name: None,
            context_tokens: Some(128_000),
            max_output_tokens: Some(8_000),
            extra_param: serde_json::Value::Null,
        }],
    };
    let config = config::ConfigHandle::from_config(config::AppConfig {
        providers: vec![base_provider],
        active_model: "base/base-model".to_string(),
        ..config::AppConfig::default()
    });
    let registration = StaticExtensionRegistration::builder()
        .providers(vec![protocol::ExtensionProviderRegistration {
            name: "extension-provider".to_string(),
            config: serde_json::json!({
                "display_name": "Extension Provider",
                "provider_type": "openai-completions",
                "base_url": "https://example.com/v1",
                "api_key": "sk-test",
                "models": [{
                    "id": "extension-model",
                    "context_tokens": 64000,
                    "max_output_tokens": 4096
                }]
            }),
        }])
        .build();

    let catalog = ProviderModelFactory::from_config(config)
        .create_catalog(&registration)
        .expect("extension model catalog");

    assert_eq!(catalog.profiles().len(), 2);
    assert_eq!(
        catalog
            .resolve("extension-provider", "extension-model")
            .expect("extension model")
            .profile()
            .display_name,
        "extension-model"
    );
    assert_eq!(catalog.active().profile().model_id, "base-model");
}

/// Cancellation interrupts an active provider backoff before another request.
#[tokio::test(start_paused = true)]
async fn cancellation_interrupts_provider_retry_backoff() {
    let llm = Arc::new(FixtureLlm::new(vec![Err(CompletionError::HttpError(
        provider::http_client::Error::InvalidStatusCode(
            http::StatusCode::SERVICE_UNAVAILABLE,
        ),
    ))]));
    let model = ProviderModel::new(
        sample_profile(),
        Arc::clone(&llm) as Arc<dyn Llm>,
        config::ProviderRetryConfig {
            timeout_ms: None,
            max_retries: Some(1),
            max_retry_delay_ms: 60_000,
        },
    );
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        model.stream(sample_request(), task_cancellation).await
    });

    tokio::task::yield_now().await;
    assert_eq!(llm.acquisition_count(), 1);
    cancellation.cancel();

    let error = match task.await.expect("cancelled retry task") {
        Ok(_) => panic!("cancelled acquisition unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(matches!(error, ModelError::Cancelled));
    assert_eq!(llm.acquisition_count(), 1);
}

/// Tool-result images cross the kernel/provider boundary as typed base64 image content.
#[tokio::test]
async fn tool_result_image_is_preserved_in_provider_request() {
    let llm =
        Arc::new(FixtureLlm::new(vec![Ok(FixtureLlm::successful_final())]));
    let model = ProviderModel::new(
        sample_profile(),
        Arc::clone(&llm) as Arc<dyn Llm>,
        config::ProviderRetryConfig::default(),
    );
    let timestamp = TimestampMs::from(200);
    let request = ModelRequest {
        messages: vec![AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("message-image")
                    .expect("message id"),
                turn_id: TurnId::try_from("turn-image").expect("turn id"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("message timing"),
            content: MessageContent::ToolResult {
                tool_call_id: protocol::ToolCallId::try_from("tool-image")
                    .expect("tool id"),
                blocks: vec![ContentBlock::Image {
                    data: "aW1hZ2U=".to_string(),
                    mime_type: "image/png".to_string(),
                }],
                is_error: false,
                details: None,
            },
        }],
        tools: Vec::new(),
    };

    let _stream = model
        .stream(request, CancellationToken::new())
        .await
        .expect("provider stream");
    let value = serde_json::to_value(
        llm.last_request().expect("captured provider request"),
    )
    .expect("serialize provider request");

    assert_eq!(
        value["chat_history"][0]["content"][0]["content"][0]["type"],
        "image"
    );
    assert_eq!(
        value["chat_history"][0]["content"][0]["content"][0]["media_type"],
        "png"
    );
    assert_eq!(
        value["chat_history"][0]["content"][0]["content"][0]["data"]["value"],
        "aW1hZ2U="
    );
}

/// Provider conversion includes `!` output and omits `!!` output from model context.
#[tokio::test]
async fn user_bash_context_projection_matches_pi_prefix_semantics() {
    let llm =
        Arc::new(FixtureLlm::new(vec![Ok(FixtureLlm::successful_final())]));
    let model = ProviderModel::new(
        sample_profile(),
        Arc::clone(&llm) as Arc<dyn Llm>,
        config::ProviderRetryConfig::default(),
    );
    let timestamp = TimestampMs::from(300);
    let messages = [("visible", false), ("private", true)]
        .into_iter()
        .enumerate()
        .map(|(index, (output, exclude_from_context))| AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(format!("bash-{index}"))
                    .expect("message id"),
                turn_id: TurnId::try_from(format!("turn-bash-{index}"))
                    .expect("turn id"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("message timing"),
            content: MessageContent::BashExecution {
                bash: BashExecutionMessage::builder()
                    .command(format!("printf '{output}'"))
                    .result(
                        UserBashResult::builder()
                            .disposition(
                                protocol::UserBashDisposition::Completed,
                            )
                            .output(output.to_string())
                            .exit_code(Some(0))
                            .cancelled(false)
                            .truncated(false)
                            .build(),
                    )
                    .exclude_from_context(exclude_from_context)
                    .build(),
            },
        })
        .collect();

    let _stream = model
        .stream(
            ModelRequest {
                messages,
                tools: Vec::new(),
            },
            CancellationToken::new(),
        )
        .await
        .expect("provider stream");
    let serialized = serde_json::to_string(
        &llm.last_request().expect("captured provider request"),
    )
    .expect("serialize provider request");

    assert!(serialized.contains("visible"));
    assert!(!serialized.contains("private"));
}
