use crate::{
    AgentMessage, ContentBlock, MessageContent, ModelInputModality,
    ModelProfile, ModelUsage, StopReason, ToolCall, ToolDefinition,
};

/// User-image placeholder sent to models without image input support.
const USER_IMAGE_PLACEHOLDER: &str =
    "(image omitted: model does not support images)";

/// Tool-image placeholder sent to models without image input support.
const TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// Optional generation controls applied only to one model request.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct ModelRequestOptions {
    /// Maximum provider output tokens requested by the caller.
    #[builder(default)]
    pub max_tokens: Option<u64>,
    /// Sampling temperature requested by the caller.
    #[builder(default)]
    pub temperature: Option<f64>,
    /// Reasoning level; Off sends no reasoning option. Required by the builder so
    /// one-off requests state Off instead of inheriting the Medium default.
    pub thinking_level: crate::ThinkingLevel,
    /// Provider prompt cache; `None` sends neither a retention nor a routing key.
    #[builder(default)]
    pub prompt_cache: Option<PromptCache>,
}

impl ModelRequestOptions {
    /// Converts the controls into rig's portable options, or `None` when nothing is set so
    /// the request is sent exactly as built. Values a provider cannot honour are skipped.
    pub fn into_generation_options(
        self,
    ) -> Option<rig_core::completion::options::GenerationOptions> {
        use rig_core::completion::options::{GenerationOptions, OnUnsupported};
        let mut options = GenerationOptions::default();
        if let Some(effort) = self.thinking_level.effort() {
            options = options.reasoning(effort);
        }
        if let Some(cache) = self.prompt_cache {
            options =
                options.cache(cache.retention.into()).cache_key(cache.key);
        }
        (!options.is_default())
            .then(|| options.on_unsupported(OnUnsupported::Ignore))
    }
}

/// One request's prompt cache: the retention to ask for and the key that routes a
/// conversation's requests to the provider server holding its cached prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptCache {
    /// Requested cache lifetime tier.
    pub retention: PromptCacheRetention,
    /// Conversation-stable routing key, such as the Session id.
    pub key: String,
}

/// Prompt-cache retention a provider is configured to request.
///
/// A project enum rather than rig's `CacheRetention` keeps the configuration schema
/// independent of pre-1.0 rig releases and leaves out `none`, which providers that
/// always cache reject on every request; omitting `prompt_cache` already sends nothing.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheRetention {
    /// The provider's short retention, typically minutes.
    Short,
    /// The provider's long retention, typically an hour or more.
    Long,
}

impl From<PromptCacheRetention>
    for rig_core::completion::options::CacheRetention
{
    /// Maps the configured tier to rig's portable retention.
    fn from(retention: PromptCacheRetention) -> Self {
        match retention {
            PromptCacheRetention::Short => Self::Short,
            PromptCacheRetention::Long => Self::Long,
        }
    }
}

/// Complete model input assembled for one pi-compatible Turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    /// Ordered active-branch transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Active tools exposed for this request.
    pub tools: Vec<ToolDefinition>,
    /// Request-local generation controls: Turns carry the Session reasoning level and,
    /// when the provider enables it, the prompt cache.
    pub options: ModelRequestOptions,
}

impl ModelRequest {
    /// Replaces unsupported images in this request without touching stored messages.
    pub fn adapt_input(&mut self, profile: &ModelProfile) -> bool {
        if profile.input.supports(ModelInputModality::Image) {
            return false;
        }

        let mut changed = false;
        for message in &mut self.messages {
            let (blocks, placeholder) = match &mut message.content {
                MessageContent::User { blocks } => {
                    (blocks, USER_IMAGE_PLACEHOLDER)
                }
                MessageContent::ExpandedUser { expansion } => {
                    (&mut expansion.model_blocks, USER_IMAGE_PLACEHOLDER)
                }
                MessageContent::ToolResult { blocks, .. } => {
                    (blocks, TOOL_IMAGE_PLACEHOLDER)
                }
                MessageContent::Extension { extension }
                    if extension.include_in_context =>
                {
                    (&mut extension.blocks, USER_IMAGE_PLACEHOLDER)
                }
                MessageContent::System { .. }
                | MessageContent::Assistant { .. }
                | MessageContent::BashExecution { .. }
                | MessageContent::Extension { .. }
                | MessageContent::SlashCommand { .. }
                | MessageContent::CompactionSummary { .. } => continue,
            };
            let mut previous_was_placeholder = false;
            let mut adapted = Vec::with_capacity(blocks.len());
            for block in std::mem::take(blocks) {
                if matches!(block, ContentBlock::Image { .. }) {
                    changed = true;
                    if !previous_was_placeholder {
                        adapted.push(ContentBlock::Text {
                            text: placeholder.to_string(),
                        });
                    }
                    previous_was_placeholder = true;
                } else {
                    previous_was_placeholder = matches!(
                        &block,
                        ContentBlock::Text { text } if text == placeholder
                    );
                    adapted.push(block);
                }
            }
            *blocks = adapted;
        }
        changed
    }
}

/// Incremental provider-neutral output consumed by the Turn state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStreamEvent {
    /// Assistant text fragment.
    TextDelta(String),
    /// Assistant reasoning fragment.
    ReasoningDelta(String),
    /// Fully parsed tool call.
    ToolCall(ToolCall),
    /// Provider stream completed with full terminal metadata.
    Finished(ModelFinal),
}

/// Complete provider-neutral terminal result for one model attempt.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct ModelFinal {
    /// Kernel-normalized terminal reason.
    pub stop_reason: StopReason,
    /// Provider-native terminal reason retained for diagnostics.
    #[builder(default)]
    pub raw_stop_reason: Option<String>,
    /// Complete provider token accounting.
    pub usage: ModelUsage,
    /// The final upstream message is authoritative over streamed display deltas.
    #[builder(default)]
    // Keep large native payloads off every stream-event stack slot.
    pub model_message: Option<Box<rig_core::message::AssistantMessage>>,
}

/// Whether the same provider request may be attempted again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRetryDisposition {
    /// The failure may be transient.
    Retryable,
    /// The same request should fail immediately.
    NonRetryable,
}

/// Safe structured details for one model request or stream failure.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct ModelFailure {
    /// Human-readable failure summary without credentials or headers.
    pub summary: String,
    /// HTTP status code when one was available.
    #[builder(default)]
    pub status: Option<u16>,
    /// Provider request retry classification.
    pub retry_disposition: ModelRetryDisposition,
    /// Provider-requested backoff is interpreted once at the model boundary.
    #[builder(default)]
    pub retry_after_ms: Option<u64>,
    /// Upstream context-overflow classification controls compaction recovery.
    #[builder(default)]
    pub context_overflow: bool,
    /// Accounting already reported by a failed terminal response; boxed to keep errors small.
    #[builder(default, setter(strip_option))]
    pub usage: Option<Box<ModelUsage>>,
}

impl ModelFailure {
    /// Classifies failed terminal responses while retaining usage and upstream failure semantics.
    pub fn from_response(
        response: &rig_core::completion::CompletionResponse,
    ) -> Option<Self> {
        use rig_core::{
            completion::FinishReason,
            message::{AssistantContent, StopReason},
        };
        let stop = response.stop();
        let finish = response.finish_reason();
        // The upstream explanation can embed provider text; use it only as a failure predicate.
        rig_core::message::turn_failure(
            &response.choice,
            Some(&stop),
            finish.as_ref(),
        )?;
        let reason = match (&stop, finish.as_ref()) {
            (_, Some(FinishReason::ContentFilter)) => {
                "content_filter (provider refused the turn)"
            }
            (StopReason::Aborted(_), _) => "aborted",
            (_, Some(FinishReason::Other(_))) => "unsupported finish reason",
            (StopReason::Error(_), _) => "provider-reported error",
            (_, Some(FinishReason::Length)) => {
                "output limit reached without a usable answer"
            }
            _ => "no usable answer",
        };
        let mut summary = format!("provider turn failed: {reason}");
        if response
            .choice
            .iter()
            .any(|content| matches!(content, AssistantContent::ToolCall(_)))
        {
            summary.push_str("; none of its tool calls ran");
        }
        Some(
            Self::builder()
                .summary(summary)
                .retry_disposition(ModelRetryDisposition::NonRetryable)
                .usage(Box::new(response.usage.into()))
                .build(),
        )
    }

    /// Uses explicit recovery facts; context overflow belongs to compaction, not request retry.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        !self.context_overflow
            && self.retry_disposition == ModelRetryDisposition::Retryable
    }
}

impl From<rig_core::ProviderError> for ModelFailure {
    /// Retains upstream recovery facts without copying response bodies into host diagnostics.
    fn from(error: rig_core::ProviderError) -> Self {
        let report = error.report();
        let retry_after_ms = report
            .retry_after(std::time::SystemTime::now())
            .map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
        let summary = match report.http_status {
            Some(status) => format!(
                "provider request failed: {} (HTTP {status})",
                report.kind.code()
            ),
            None => format!("provider request failed: {}", report.kind.code()),
        };
        Self::builder()
            .summary(summary)
            .status(report.http_status)
            .retry_disposition(if report.is_retryable() {
                ModelRetryDisposition::Retryable
            } else {
                ModelRetryDisposition::NonRetryable
            })
            .retry_after_ms(retry_after_ms)
            .context_overflow(report.is_context_overflow())
            .build()
    }
}
