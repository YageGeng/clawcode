use protocol::{
    AgentMessage, ContentBlock, MessageContent, ModelProfile,
    ModelRequestOptions, ModelUsage, StopReason,
};

use super::*;

mod context;
mod lifecycle;
mod prompt;

pub use context::ContextUsageEstimate;
pub(in crate::runtime) use context::{
    CompactionPolicyExt, CompactionPreparation, EstimatedTokens,
    OverflowRecovery,
};
use lifecycle::CompactionLifecycle;
pub(in crate::runtime) use prompt::{
    GeneratedSummary, SummaryGeneration, SummaryProtocol,
};
use prompt::{SUMMARIZATION_SYSTEM_PROMPT, SummaryPrompt, SummaryTemplate};

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
    /// Optional identity supplied by a containing Slash Command operation.
    #[builder(default)]
    identity: Option<CompactionIdentity>,
    /// Optional caller-provided model instruction for manual compaction.
    #[builder(default)]
    instruction: Option<String>,
}

/// Stable operation identity used by every compaction lifecycle event.
#[derive(Clone)]
pub(in crate::runtime) struct CompactionIdentity {
    run_id: RunId,
    turn_id: TurnId,
    started_at_ms: TimestampMs,
}

impl CompactionIdentity {
    /// Creates a compaction identity from already validated operation values.
    pub(in crate::runtime) fn new(
        run_id: RunId,
        turn_id: TurnId,
        started_at_ms: TimestampMs,
    ) -> Self {
        Self {
            run_id,
            turn_id,
            started_at_ms,
        }
    }
}

impl Kernel {
    /// Selects pre-prompt compaction from the persisted active history and model limits.
    pub(super) fn pre_prompt_compaction(
        &self,
        session: &SessionRuntime,
        profile: &ModelProfile,
    ) -> Result<Option<(CompactionReason, Option<MessageId>)>, KernelError>
    {
        let history = session
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let latest_compaction_started_at =
            history.iter().rev().find_map(|message| {
                matches!(
                    message.content,
                    MessageContent::CompactionSummary { .. }
                )
                .then_some(message.timing.started_at_ms)
            });
        // Retained-tail messages are ordered after the synthetic summary but
        // retain their original timing, so stale provider metadata must not
        // initiate another overflow recovery after the same compaction.
        let latest_assistant = history.iter().rev().find_map(|message| {
            if let MessageContent::Assistant { metadata, .. } = &message.content
                && latest_compaction_started_at.is_none_or(|boundary| {
                    message.timing.ended_at_ms > boundary
                })
            {
                Some((message, metadata))
            } else {
                None
            }
        });
        Ok(latest_assistant
            .and_then(|(message, metadata)| {
                self.compaction_policy
                    .overflow_recovery(metadata, profile)
                    .map(|recovery| {
                        let excluded = (recovery
                            == OverflowRecovery::CompactAndRetry)
                            .then(|| message.identity.message_id.clone());
                        (CompactionReason::Overflow, excluded)
                    })
            })
            .or_else(|| {
                ContextUsageEstimate::from_history(&history)
                    .should_compact(
                        profile.context_tokens,
                        self.compaction_policy,
                    )
                    .then_some((CompactionReason::Threshold, None))
            }))
    }

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
        let _run_guard = session.acquire_operation().await?;
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
            session_id: _session_id,
            session,
            sink,
            reason,
            cancellation,
            excluded_message_id,
            identity,
            instruction,
        } = execution;
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
        // Direct command records are intentionally model-invisible. Rejecting
        // before lifecycle hooks and records prevents a false compaction run.
        if !preparation
            .summarized
            .iter()
            .chain(&preparation.turn_prefix)
            .any(|message| message.estimated_tokens() > 0)
        {
            return Err(KernelError::NoContextToCompact);
        }
        let identity = match identity {
            Some(identity) => identity,
            None => CompactionIdentity::new(
                RunId::try_from(self.id_generator.next(IdKind::Run)).map_err(
                    |error| KernelError::Protocol(error.to_string()),
                )?,
                TurnId::try_from(self.id_generator.next(IdKind::Turn))
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                self.clock.now(),
            ),
        };
        let CompactionIdentity {
            run_id,
            turn_id,
            started_at_ms,
        } = identity;
        let entry_id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let will_retry = reason == CompactionReason::Overflow
            && excluded_message_id.is_some();
        let extension_context =
            self.extension_context(session, Some(&run_id), Some(&turn_id))?;
        let extension_result = session
            .extensions
            .emit_session_before_compact(
                &protocol::SessionBeforeCompactEvent::builder()
                    .reason(reason)
                    .will_retry(will_retry)
                    .branch_entries(
                        extension_context.snapshot.tree.entries.clone(),
                    )
                    .build(),
                &extension_context,
            )
            .await;
        if extension_result.cancel {
            return Err(KernelError::ExtensionBlocked(
                "session compaction cancelled".to_string(),
            ));
        }
        let extension_compaction = extension_result.compaction;
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink,
            session: Arc::clone(session),
        };
        let source_leaf_id = session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .lane(&session.lane)
            .cloned();
        self.record_operation(
            session,
            &run_id,
            RecordKind::OperationStarted,
            serde_json::to_value(protocol::CompactionOperationStarted {
                source_leaf_id,
                intent: protocol::CompactionOperationIntent::new(
                    instruction.clone(),
                    entry_id.clone(),
                ),
            })?,
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
        let lifecycle = CompactionLifecycle::builder()
            .kernel(self)
            .session(session.as_ref())
            .emitter(&emitter)
            .run_id(&run_id)
            .turn_id(&turn_id)
            .reason(reason)
            .build();
        let from_extension = extension_compaction.is_some();
        let compaction_result: Result<CompactionResult, KernelError> = async {
            self.record_operation(
                session,
                &run_id,
                RecordKind::StepAttempt,
                serde_json::to_value(
                    protocol::CompactionStepAttempt::builder()
                        .step(protocol::CompactionStep::Compaction)
                        .attempt(1)
                        .result_entry_id(entry_id.clone())
                        .compaction_reason(reason)
                        .build(),
                )?,
            )?;
            let CompactionPreparation {
                summarized,
                turn_prefix,
                retained_tail,
                tokens_before,
                previous_summary,
                file_operations,
            } = preparation;
            let (summary, retained_tail, tokens_before, summary_usage) =
                if let Some(compaction) = extension_compaction {
                    (
                        compaction.summary,
                        compaction.retained_tail,
                        compaction.tokens_before,
                        None,
                    )
                } else {
                    let generated = async {
                        if turn_prefix.is_empty() {
                            return self
                                .generate_compaction_summary(
                                    SummaryGeneration::builder()
                                        .session(session)
                                        .run_id(&run_id)
                                        .turn_id(&turn_id)
                                        .timestamp(started_at_ms)
                                        .messages(summarized)
                                        .cancellation(cancellation)
                                        .protocol(SummaryProtocol::Compaction {
                                            previous_summary:
                                                previous_summary.as_deref(),
                                            custom_instructions:
                                                instruction.as_deref(),
                                        })
                                        .build(),
                                )
                                .await;
                        }
                        let history_summary = if summarized.is_empty() {
                            None
                        } else {
                            Some(
                                self.generate_compaction_summary(
                                    SummaryGeneration::builder()
                                        .session(session)
                                        .run_id(&run_id)
                                        .turn_id(&turn_id)
                                        .timestamp(started_at_ms)
                                        .messages(summarized)
                                        .cancellation(cancellation)
                                        .protocol(SummaryProtocol::Compaction {
                                            previous_summary:
                                                previous_summary.as_deref(),
                                            custom_instructions:
                                                instruction.as_deref(),
                                        })
                                        .build(),
                                )
                                .await?,
                            )
                        };
                        let prefix_summary = self
                            .generate_compaction_summary(
                                SummaryGeneration::builder()
                                    .session(session)
                                    .run_id(&run_id)
                                    .turn_id(&turn_id)
                                    .timestamp(started_at_ms)
                                    .messages(turn_prefix)
                                    .cancellation(cancellation)
                                    .protocol(SummaryProtocol::TurnPrefix)
                                    .build(),
                            )
                            .await?;
                        let history_text = history_summary
                            .as_ref()
                            .map_or("No prior history.", |summary| {
                                summary.text.as_str()
                            });
                        let usage = history_summary.as_ref().map_or_else(
                            || prefix_summary.usage.clone(),
                            |history| {
                                ModelUsage::builder()
                                    .input_tokens(
                                        history
                                            .usage
                                            .input_tokens
                                            .saturating_add(
                                                prefix_summary.usage.input_tokens,
                                            ),
                                    )
                                    .output_tokens(
                                        history
                                            .usage
                                            .output_tokens
                                            .saturating_add(
                                                prefix_summary.usage.output_tokens,
                                            ),
                                    )
                                    .cache_read_tokens(
                                        history
                                            .usage
                                            .cache_read_tokens
                                            .saturating_add(
                                                prefix_summary
                                                    .usage
                                                    .cache_read_tokens,
                                            ),
                                    )
                                    .cache_write_tokens(
                                        history
                                            .usage
                                            .cache_write_tokens
                                            .saturating_add(
                                                prefix_summary
                                                    .usage
                                                    .cache_write_tokens,
                                            ),
                                    )
                                    .reasoning_tokens(match (
                                        history.usage.reasoning_tokens,
                                        prefix_summary.usage.reasoning_tokens,
                                    ) {
                                        (None, None) => None,
                                        (left, right) => Some(
                                            left.unwrap_or(0)
                                                .saturating_add(right.unwrap_or(0)),
                                        ),
                                    })
                                    .total_tokens(
                                        history
                                            .usage
                                            .total_tokens
                                            .saturating_add(
                                                prefix_summary.usage.total_tokens,
                                            ),
                                    )
                                    .build()
                            },
                        );
                        Ok(GeneratedSummary {
                            text: format!(
                                "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
                                prefix_summary.text
                            ),
                            usage,
                            stop_reason: prefix_summary.stop_reason,
                        })
                    }
                    .await;
                    let generated = generated?;
                    self.record_operation(
                        session,
                        &run_id,
                        RecordKind::Usage,
                        serde_json::to_value(
                            protocol::CompactionUsageRecord::builder()
                                .cause(protocol::CompactionUsageCause::Compaction)
                                .entry_id(entry_id.clone())
                                .attempt(1)
                                .stop_reason(generated.stop_reason)
                                .usage(generated.usage.clone())
                                .build(),
                        )?,
                    )?;
                    (
                        generated.text,
                        retained_tail,
                        tokens_before,
                        Some(generated.usage),
                    )
                };
            let read_files = file_operations.read_files();
            let modified_files = file_operations.modified_files();
            // Pi keeps cumulative file operations in both typed details and the
            // model-visible summary so future Turns retain filesystem context.
            let mut summary = summary;
            if !read_files.is_empty() {
                summary.push_str("\n\n<read-files>\n");
                summary.push_str(&read_files.join("\n"));
                summary.push_str("\n</read-files>");
            }
            if !modified_files.is_empty() {
                summary.push_str("\n\n<modified-files>\n");
                summary.push_str(&modified_files.join("\n"));
                summary.push_str("\n</modified-files>");
            }
            let ended_at_ms = self.clock.now();
            let details = CompactionDetails::builder()
                .reason(reason)
                .run_id(run_id.clone())
                .turn_id(turn_id.clone())
                .started_at_ms(started_at_ms)
                .ended_at_ms(ended_at_ms)
                .read_files(read_files)
                .modified_files(modified_files)
                .build();
            let data = CompactionData::builder()
                .summary(summary.clone())
                .retained_tail(retained_tail.clone())
                .tokens_before(tokens_before)
                .usage(summary_usage.clone())
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
                content: MessageContent::CompactionSummary {
                    compaction: protocol::CompactionSummaryMessage::builder()
                        .entry_id(entry_id.clone())
                        .summary(summary.clone())
                        .tokens_before(tokens_before)
                        .read_files(details.read_files.clone())
                        .modified_files(details.modified_files.clone())
                        .build(),
                },
            };
            let mut context =
                Vec::with_capacity(retained_tail.len().saturating_add(1));
            context.push(summary_message);
            context.extend(retained_tail);
            *session
                .history
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)? = context;
            let result = CompactionResult::builder()
                .entry_id(entry_id.clone())
                .turn_id(turn_id.clone())
                .summary(summary)
                .tokens_before(tokens_before)
                .usage(summary_usage)
                .started_at_ms(started_at_ms)
                .ended_at_ms(ended_at_ms)
                .build();
                Ok(result)
        }
        .await;
        let result = match compaction_result {
            Ok(result) => result,
            Err(error) => {
                if let Err(terminal_error) = lifecycle.fail(&error).await {
                    tracing::error!(
                        "failed to settle compaction Run {} after execution failure {}: {}",
                        run_id,
                        error,
                        terminal_error
                    );
                }
                return Err(error);
            }
        };
        let terminal_result = lifecycle.complete(&result).await;
        session
            .extensions
            .emit_session_compact(
                &protocol::SessionCompactEvent::builder()
                    .compaction(result.clone())
                    .from_extension(from_extension)
                    .reason(reason)
                    .will_retry(will_retry)
                    .build(),
                &extension_context,
            )
            .await;
        terminal_result?;
        Ok(result)
    }

    /// Streams a tool-free summary request and rejects empty or tool-producing output.
    pub(in crate::runtime) async fn generate_compaction_summary(
        &self,
        generation: SummaryGeneration<'_>,
    ) -> Result<GeneratedSummary, KernelError> {
        let SummaryGeneration {
            session,
            run_id,
            turn_id,
            timestamp,
            messages,
            cancellation,
            protocol,
        } = generation;
        let (system_prompt, user_prompt, max_tokens) = match protocol {
            SummaryProtocol::Compaction {
                previous_summary,
                custom_instructions,
            } => (
                SUMMARIZATION_SYSTEM_PROMPT,
                Some(
                    SummaryPrompt {
                        messages: &messages,
                        previous_summary,
                        custom_instructions,
                        template: SummaryTemplate::Checkpoint,
                    }
                    .render(),
                ),
                Some(
                    self.compaction_policy
                        .reserve_tokens
                        .saturating_mul(8)
                        .checked_div(10)
                        .unwrap_or(0),
                ),
            ),
            SummaryProtocol::TurnPrefix => (
                SUMMARIZATION_SYSTEM_PROMPT,
                Some(
                    SummaryPrompt {
                        messages: &messages,
                        previous_summary: None,
                        custom_instructions: None,
                        template: SummaryTemplate::TurnPrefix,
                    }
                    .render(),
                ),
                Some(
                    self.compaction_policy
                        .reserve_tokens
                        .checked_div(2)
                        .unwrap_or(0),
                ),
            ),
            SummaryProtocol::Branch { instruction } => {
                (instruction, None, None)
            }
        };
        let system_message = AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id()?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::System {
                blocks: vec![ContentBlock::Text {
                    text: system_prompt.to_string(),
                }],
            },
        };
        let mut request_messages = Vec::with_capacity(messages.len() + 1);
        request_messages.push(system_message);
        if let Some(user_prompt) = user_prompt {
            request_messages.push(AgentMessage {
                identity: MessageIdentity {
                    message_id: self.message_id()?,
                    turn_id: turn_id.clone(),
                },
                timing: MessageTiming::try_from((
                    timestamp, timestamp, timestamp,
                ))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
                content: MessageContent::User {
                    blocks: vec![ContentBlock::Text { text: user_prompt }],
                },
            });
        } else {
            request_messages.extend(messages);
        }
        let completion_hooks =
            self.completion_hooks(session, run_id, turn_id)?;
        // Compaction uses one immutable model snapshot just like a normal Turn.
        let model = session
            .model
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .clone();
        model.preflight().await?;
        let max_tokens = max_tokens
            .map(|tokens| tokens.min(model.profile().max_output_tokens));
        let request = ModelRequest {
            messages: request_messages,
            tools: Vec::new(),
            options: ModelRequestOptions {
                max_tokens,
                temperature: None,
            },
        };
        let mut retry_state = RetryState::default();
        loop {
            let result: Result<GeneratedSummary, ModelError> = async {
                let mut stream = model
                    .stream_with_hooks(
                        request.clone(),
                        cancellation.clone(),
                        Some(Arc::clone(&completion_hooks)),
                    )
                    .await?;
                let mut summary = String::new();
                let mut final_result = None;
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
                        ModelStreamEvent::ReasoningDelta(_) => {}
                        ModelStreamEvent::Finished(result) => {
                            final_result = Some(result);
                        }
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
                let final_result = final_result.ok_or_else(|| {
                    ModelError::Protocol(
                        "compaction model returned no terminal usage"
                            .to_string(),
                    )
                })?;
                match final_result.stop_reason {
                    StopReason::Cancelled => return Err(ModelError::Cancelled),
                    StopReason::Error | StopReason::Refusal => {
                        return Err(ModelError::Protocol(
                            "compaction model returned an error outcome"
                                .to_string(),
                        ));
                    }
                    StopReason::ToolUse => {
                        return Err(ModelError::Protocol(
                            "compaction model returned a tool-use outcome"
                                .to_string(),
                        ));
                    }
                    StopReason::EndTurn | StopReason::MaxTokens => {}
                }
                Ok(GeneratedSummary {
                    text: summary,
                    usage: final_result.usage,
                    stop_reason: final_result.stop_reason,
                })
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
