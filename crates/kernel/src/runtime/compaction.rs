use protocol::{
    AgentMessage, AssistantMetadata, CompactionPolicy, ContentBlock,
    MessageContent, ModelProfile, ModelUsage, StopReason,
};

use super::*;

/// Borrowed execution context shared by manual, threshold, and overflow compaction paths.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct CompactionExecution<'a> {
    /// Session identifier supplied to extension lifecycle events.
    session_id: &'a SessionId,
    /// Live session whose caller already owns the serialized run lock.
    session: &'a Arc<SessionRuntime>,
    /// Event destination shared with the surrounding agent operation.
    sink: Arc<dyn EventSink>,
    /// Persisted and emitted reason for this compaction.
    reason: CompactionReason,
    /// Cancellation token owned by the surrounding operation.
    cancellation: &'a CancellationToken,
    /// Failed or truncated Assistant omitted from recovered active context.
    #[builder(default)]
    excluded_message_id: Option<&'a MessageId>,
}

impl Kernel {
    /// Generates and persists one model-backed pi v4 compaction atomically on failure.
    pub async fn compact_session(
        &self,
        session_id: &SessionId,
        sink: Arc<dyn EventSink>,
    ) -> Result<CompactionResult, KernelError> {
        self.compact_session_with_reason(
            session_id,
            sink,
            CompactionReason::Manual,
        )
        .await
    }

    /// Runs one serialized compaction while preserving its trigger reason in events and Store.
    async fn compact_session_with_reason(
        &self,
        session_id: &SessionId,
        sink: Arc<dyn EventSink>,
        reason: CompactionReason,
    ) -> Result<CompactionResult, KernelError> {
        let session = self.session(session_id)?;
        let _run_guard = session.run_gate.lock().await;
        let cancellation = CancellationToken::new();
        *session
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            cancellation.clone();
        self.perform_compaction(
            CompactionExecution::builder()
                .session_id(session_id)
                .session(&session)
                .sink(sink)
                .reason(reason)
                .cancellation(&cancellation)
                .build(),
        )
        .await
    }

    /// Performs compaction under the caller-owned session run lock for manual and automatic paths.
    pub(super) async fn perform_compaction(
        &self,
        execution: CompactionExecution<'_>,
    ) -> Result<CompactionResult, KernelError> {
        let CompactionExecution {
            session_id,
            session,
            sink,
            reason,
            cancellation,
            excluded_message_id,
        } = execution;
        self.dispatch(ExtensionEvent::SessionBeforeCompact, session_id, None)
            .await?;
        let run_id = RunId::try_from(self.id_generator.next(IdKind::Run))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let turn_id = TurnId::try_from(self.id_generator.next(IdKind::Turn))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let entry_id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let started_at_ms = self.clock.now();
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink,
            session: Arc::clone(session),
        };
        self.record_operation(
            session,
            &run_id,
            RecordKind::OperationStarted,
            serde_json::json!({
                "operation": "compaction",
                "resultEntryId": entry_id,
            }),
        )?;
        emitter
            .emit_at(
                turn_id.clone(),
                started_at_ms,
                AgentEventPayload::CompactionStart {
                    run_id: run_id.clone(),
                    reason,
                },
            )
            .await?;
        let mut compacted_history = session
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .clone();
        // Failed and explicitly excluded Assistant attempts stay replayable in
        // Store but never become part of a recovered active model context.
        compacted_history.retain(|message| {
            !matches!(
                &message.content,
                MessageContent::Assistant { metadata, .. }
                    if metadata.error.is_some()
                        || excluded_message_id
                            .is_some_and(|id| id == &message.identity.message_id)
            )
        });
        let preparation = CompactionPreparation::from_history(
            &compacted_history,
            self.compaction_policy,
        );
        let summary = match self
            .generate_compaction_summary(
                &turn_id,
                started_at_ms,
                preparation.summarized,
                cancellation,
            )
            .await
        {
            Ok(summary) => summary,
            Err(error) => {
                self.record_operation(
                    session,
                    &run_id,
                    RecordKind::OperationFinished,
                    serde_json::json!({
                        "status": "failed",
                        "error": error.to_string(),
                    }),
                )?;
                return Err(error);
            }
        };
        let ended_at_ms = self.clock.now();
        let details = CompactionDetails::builder()
            .reason(reason)
            .turn_id(turn_id.clone())
            .started_at_ms(started_at_ms)
            .ended_at_ms(ended_at_ms)
            .build();
        let data = CompactionData::builder()
            .summary(summary.clone())
            .retained_tail(preparation.retained_tail.clone())
            .tokens_before(preparation.tokens_before)
            .details(Some(details.clone()))
            .build();
        session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .append_entry(
                &session.lane,
                NewEntry {
                    id: entry_id.clone(),
                    kind: EntryKind::Compaction,
                    payload: serde_json::to_value(&data)?,
                },
            )?;
        let summary_message = AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(format!(
                    "compaction-summary-{entry_id}"
                ))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((
                started_at_ms,
                started_at_ms,
                ended_at_ms,
            ))
            .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::System {
                blocks: vec![ContentBlock::Text { text: summary }],
            },
        };
        let mut context = Vec::with_capacity(
            preparation.retained_tail.len().saturating_add(1),
        );
        context.push(summary_message);
        context.extend(preparation.retained_tail);
        *session
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? = context;
        let result = CompactionResult::builder()
            .entry_id(entry_id.clone())
            .turn_id(turn_id.clone())
            .started_at_ms(started_at_ms)
            .ended_at_ms(ended_at_ms)
            .build();
        self.record_operation(
            session,
            &run_id,
            RecordKind::StepAttempt,
            serde_json::json!({
                "step": "compaction",
                "resultEntryId": entry_id,
                "compactionReason": reason,
            }),
        )?;
        self.record_operation(
            session,
            &run_id,
            RecordKind::OperationFinished,
            serde_json::json!({ "status": "completed" }),
        )?;
        emitter
            .emit_at(
                turn_id,
                ended_at_ms,
                AgentEventPayload::CompactionEnd {
                    run_id,
                    reason,
                    result: result.clone(),
                },
            )
            .await?;
        self.dispatch(ExtensionEvent::SessionCompact, session_id, None)
            .await?;
        Ok(result)
    }

    /// Streams a tool-free summary request and rejects empty or tool-producing output.
    async fn generate_compaction_summary(
        &self,
        turn_id: &TurnId,
        timestamp: TimestampMs,
        messages: Vec<AgentMessage>,
        cancellation: &CancellationToken,
    ) -> Result<String, KernelError> {
        let instruction = AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id()?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::System {
                blocks: vec![ContentBlock::Text {
                    text: COMPACTION_INSTRUCTION.to_string(),
                }],
            },
        };
        let mut request_messages = Vec::with_capacity(messages.len() + 1);
        request_messages.push(instruction);
        request_messages.extend(messages);
        let request = ModelRequest {
            messages: request_messages,
            tools: Vec::new(),
        };
        let mut retry_state = RetryState::default();
        loop {
            let result: Result<String, ModelError> = async {
                let mut stream = self
                    .model
                    .stream(request.clone(), cancellation.clone())
                    .await?;
                let mut summary = String::new();
                loop {
                    let item = tokio::select! {
                        () = cancellation.cancelled() => {
                            return Err(ModelError::Cancelled);
                        }
                        item = stream.next() => item,
                    };
                    let Some(item) = item else {
                        break;
                    };
                    match item? {
                        ModelStreamEvent::TextDelta(delta) => {
                            summary.push_str(&delta);
                        }
                        ModelStreamEvent::ReasoningDelta(_)
                        | ModelStreamEvent::Finished(_) => {}
                        ModelStreamEvent::ToolCall(_call) => {
                            return Err(ModelError::Protocol(
                                "compaction model returned a tool call"
                                    .to_string(),
                            ));
                        }
                    }
                }
                let summary = summary.trim().to_string();
                if summary.is_empty() {
                    return Err(ModelError::Protocol(
                        "compaction summary was empty".to_string(),
                    ));
                }
                Ok(summary)
            }
            .await;

            let error = match result {
                Ok(summary) => return Ok(summary),
                Err(error) => error,
            };
            let retryable = match &error {
                ModelError::Preflight(failure)
                | ModelError::Request(failure)
                | ModelError::Stream(failure) => {
                    RetryClassifier::classify(failure)
                        == protocol::ModelRetryDisposition::Retryable
                }
                ModelError::Unavailable(_)
                | ModelError::Protocol(_)
                | ModelError::Cancelled => false,
            };
            let Some(schedule) = retryable
                .then(|| retry_state.schedule(&self.retry_policy))
                .flatten()
            else {
                return Err(error.into());
            };
            let delay =
                tokio::time::sleep(Duration::from_millis(schedule.delay_ms));
            tokio::pin!(delay);
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(ModelError::Cancelled.into());
                }
                () = &mut delay => {}
            }
        }
    }
}

/// Fixed model instruction used for provider-neutral context summarization.
const COMPACTION_INSTRUCTION: &str = "Summarize the conversation context for another agent. Preserve user requirements, decisions, completed work, unresolved problems, exact identifiers, and important technical details. Return only the summary.";

/// Provides kernel-specific overflow classification for shared compaction policy data.
pub(super) trait CompactionPolicyExt {
    /// Classifies pi-compatible overflow signals and whether they need one resumed Turn.
    fn overflow_recovery(
        &self,
        metadata: &AssistantMetadata,
        profile: &ModelProfile,
    ) -> Option<OverflowRecovery>;
}

impl CompactionPolicyExt for CompactionPolicy {
    /// Classifies pi-compatible overflow signals and whether they need one resumed Turn.
    fn overflow_recovery(
        &self,
        metadata: &AssistantMetadata,
        profile: &ModelProfile,
    ) -> Option<OverflowRecovery> {
        if !self.enabled
            || metadata.provider_id != profile.provider_id
            || metadata.model_id != profile.model_id
        {
            return None;
        }

        let input_tokens = metadata
            .usage
            .input_tokens
            .saturating_add(metadata.usage.cache_read_tokens);
        if metadata.stop_reason == StopReason::EndTurn
            && input_tokens > profile.context_tokens
        {
            return Some(OverflowRecovery::CompactOnly);
        }
        if metadata.stop_reason == StopReason::MaxTokens
            && (metadata.usage.output_tokens < profile.max_output_tokens
                || input_tokens.saturating_mul(100)
                    >= profile.context_tokens.saturating_mul(99))
        {
            return Some(OverflowRecovery::CompactAndRetry);
        }
        if metadata.stop_reason != StopReason::Error {
            return None;
        }

        let error = metadata.error.as_deref()?.to_ascii_lowercase();
        if [
            "throttling error:",
            "service unavailable:",
            "rate limit",
            "too many requests",
        ]
        .iter()
        .any(|pattern| error.contains(pattern))
        {
            return None;
        }
        [
            "prompt is too long",
            "request_too_large",
            "input is too long for requested model",
            "exceeds the context window",
            "maximum context length",
            "input token count",
            "maximum prompt length",
            "reduce the length of the messages",
            "maximum allowed input length",
            "exceeds the available context size",
            "greater than the context length",
            "context window exceeds limit",
            "exceeded model token limit",
            "too large for model with",
            "configured context size",
            "model_context_window_exceeded",
            "prompt too long",
            "range of input length should be",
            "context_length_exceeded",
            "context length exceeded",
            "too many tokens",
            "token limit exceeded",
            "400 status code (no body)",
            "413 status code (no body)",
        ]
        .iter()
        .any(|pattern| error.contains(pattern))
        .then_some(OverflowRecovery::CompactAndRetry)
    }
}

/// Bounded recovery action inferred from one completed Assistant attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverflowRecovery {
    /// Compact a silently over-window successful response without repeating it.
    CompactOnly,
    /// Remove the failed/truncated Assistant, compact, and start one new Turn.
    CompactAndRetry,
}

/// Provider-grounded estimate of the current active context size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated complete context tokens.
    pub tokens: u64,
    /// Tokens reported by the latest valid Assistant usage block.
    pub usage_tokens: u64,
    /// Estimated tokens after the usage-providing Assistant.
    pub trailing_tokens: u64,
    /// Message index that supplied usage, or `None` when no valid usage exists.
    pub last_usage_index: Option<usize>,
}

impl ContextUsageEstimate {
    /// Uses the latest successful Assistant usage plus estimated trailing messages.
    #[must_use]
    pub fn from_history(history: &[AgentMessage]) -> Self {
        let compaction_timestamp = history.iter().rev().find_map(|message| {
            matches!(message.content, MessageContent::System { .. })
                .then_some(message.timing.timestamp_ms)
        });
        let latest =
            history
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, message)| match &message.content {
                    MessageContent::Assistant { metadata, .. }
                        if !matches!(
                            metadata.stop_reason,
                            StopReason::Error | StopReason::Cancelled
                        ) && metadata.usage.context_tokens() > 0
                            && compaction_timestamp.is_none_or(
                                |boundary| {
                                    message.timing.timestamp_ms > boundary
                                },
                            ) =>
                    {
                        Some((index, metadata.usage.context_tokens()))
                    }
                    MessageContent::System { .. }
                    | MessageContent::User { .. }
                    | MessageContent::Assistant { .. }
                    | MessageContent::ToolResult { .. }
                    | MessageContent::Extension { .. } => None,
                });

        if let Some((index, usage_tokens)) = latest {
            let trailing_tokens = history
                .get(index.saturating_add(1)..)
                .unwrap_or_default()
                .iter()
                .fold(0_u64, |total, message| {
                    total.saturating_add(message.estimated_tokens())
                });
            return Self {
                tokens: usage_tokens.saturating_add(trailing_tokens),
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(index),
            };
        }

        let estimated = history.iter().fold(0_u64, |total, message| {
            total.saturating_add(message.estimated_tokens())
        });
        Self {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        }
    }

    /// Returns whether usage crossed the configured context reserve threshold.
    #[must_use]
    pub fn should_compact(
        &self,
        context_window: u64,
        policy: CompactionPolicy,
    ) -> bool {
        policy.enabled
            && self.tokens
                > context_window.saturating_sub(policy.reserve_tokens)
    }
}

/// Model input partition and deterministic pre-compaction token estimate.
struct CompactionPreparation {
    summarized: Vec<AgentMessage>,
    retained_tail: Vec<AgentMessage>,
    tokens_before: u64,
}

impl CompactionPreparation {
    /// Retains approximately the configured recent tokens without splitting a Turn.
    fn from_history(
        history: &[AgentMessage],
        policy: CompactionPolicy,
    ) -> Self {
        let split = if policy.keep_recent_tokens == 0 {
            history.len()
        } else {
            let mut retained_tokens = 0_u64;
            let mut boundary_turn = None;
            for message in history.iter().rev() {
                retained_tokens =
                    retained_tokens.saturating_add(message.estimated_tokens());
                boundary_turn = Some(message.identity.turn_id.clone());
                if retained_tokens >= policy.keep_recent_tokens {
                    break;
                }
            }
            boundary_turn.map_or(0, |turn_id| {
                history
                    .iter()
                    .position(|message| message.identity.turn_id == turn_id)
                    .unwrap_or(0)
            })
        };
        let (summarized, retained_tail) =
            history.split_at_checked(split).unwrap_or((history, &[]));

        Self {
            summarized: summarized.to_vec(),
            retained_tail: retained_tail.to_vec(),
            tokens_before: ContextUsageEstimate::from_history(history).tokens,
        }
    }
}

/// Token-accounting projection for provider usage.
trait ContextTokens {
    /// Returns provider total tokens or a checked component fallback.
    fn context_tokens(&self) -> u64;
}

impl ContextTokens for ModelUsage {
    fn context_tokens(&self) -> u64 {
        if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.input_tokens
                .saturating_add(self.output_tokens)
                .saturating_add(self.cache_read_tokens)
                .saturating_add(self.cache_write_tokens)
        }
    }
}

/// Conservative four-characters-per-token projection for trailing messages.
trait EstimatedTokens {
    /// Estimates one complete message without provider tokenizer access.
    fn estimated_tokens(&self) -> u64;
}

impl EstimatedTokens for AgentMessage {
    fn estimated_tokens(&self) -> u64 {
        let blocks = match &self.content {
            MessageContent::System { blocks }
            | MessageContent::User { blocks }
            | MessageContent::Assistant { blocks, .. }
            | MessageContent::ToolResult { blocks, .. } => blocks,
            MessageContent::Extension { extension } => {
                let characters = serde_json::to_string(extension)
                    .map_or(0_usize, |value| value.chars().count());
                return u64::try_from(characters.div_ceil(4))
                    .unwrap_or(u64::MAX);
            }
        };
        let characters = blocks.iter().fold(0_usize, |total, block| {
            let block_characters = match block {
                ContentBlock::Text { text }
                | ContentBlock::Reasoning { text } => text.chars().count(),
                // Pi assigns each image a fixed 4,800-character estimate.
                ContentBlock::Image { .. } => 4_800,
                ContentBlock::ToolCall {
                    name, arguments, ..
                } => name.chars().count().saturating_add(
                    serde_json::to_string(arguments)
                        .map_or(0_usize, |value| value.chars().count()),
                ),
            };
            total.saturating_add(block_characters)
        });
        u64::try_from(characters.div_ceil(4)).unwrap_or(u64::MAX)
    }
}
