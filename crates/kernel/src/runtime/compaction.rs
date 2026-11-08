mod context;
mod lifecycle;
mod prompt;
pub use context::ContextUsageEstimate;
pub(in crate::runtime) use context::{
    CompactionFileOperations, CompactionPolicyExt, CompactionPreparation,
    EstimatedTokens, OverflowRecovery,
};
pub(in crate::runtime) use prompt::{
    BRANCH_SUMMARY_PREAMBLE, GeneratedSummary, SummaryGeneration,
    SummaryProtocol,
};

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use lifecycle::CompactionLifecycle;
use prompt::{SUMMARIZATION_SYSTEM_PROMPT, SummaryPrompt, SummaryTemplate};
use protocol::{
    AgentEventPayload, AgentMessage, CompactionData, CompactionDetails,
    CompactionReason, CompactionResult, ContentBlock, EntryId, IdKind,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ModelProfile,
    ModelRequest, ModelRequestOptions, ModelStreamEvent, RunId, SessionId,
    StopReason, TimestampMs, TurnId,
};
use store::{EntryKind, NewEntry, RecordKind};
use tokio_util::sync::CancellationToken;

use crate::runtime::run::retry::RetryState;
use crate::runtime::{EventEmitter, Kernel, KernelError, RunSpan, Session};
use crate::{EventSink, ModelError};

/// Borrowed execution context shared by manual, threshold, and overflow compaction paths.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct CompactionExecution<'a> {
    /// Session identifier supplied to extension lifecycle events.
    session_id: &'a SessionId,
    /// Live session whose caller already owns the serialized run lock.
    session: &'a Arc<Session>,
    /// Event destination shared with the surrounding agent operation.
    sink: Arc<dyn EventSink>,
    /// Persisted and emitted reason for this compaction.
    reason: CompactionReason,
    /// Cancellation token owned by the surrounding operation.
    cancellation: &'a CancellationToken,
    /// Failed or truncated Assistant omitted from recovered active context.
    #[builder(default)]
    excluded_message_id: Option<&'a MessageId>,
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

/// Sizes summary output like pi: the model's output limit, reduced to fit the
/// window left after the summary request, with a safety margin for input
/// counted higher than estimated.
fn summary_max_tokens(profile: &ModelProfile, input_tokens: u64) -> u64 {
    const CONTEXT_SAFETY_TOKENS: u64 = 4_096;
    const MIN_OUTPUT_WITH_RATIO_MARGIN: u64 = 32_768;
    let context = profile.context_tokens;
    // The 15% margin stays only while it leaves at least 32,768 output tokens;
    // otherwise only the fixed margin is kept and output is capped there.
    let margin = CONTEXT_SAFETY_TOKENS
        .max(input_tokens.saturating_mul(15).div_ceil(100));
    let mut room = context.saturating_sub(input_tokens).saturating_sub(margin);
    if room < MIN_OUTPUT_WITH_RATIO_MARGIN {
        room = MIN_OUTPUT_WITH_RATIO_MARGIN.min(
            context
                .saturating_sub(input_tokens)
                .saturating_sub(CONTEXT_SAFETY_TOKENS),
        );
    }
    profile.max_output_tokens.min(room.max(1))
}

impl Kernel {
    /// Selects pre-prompt compaction from the persisted active history and model limits.
    pub(super) fn pre_prompt_compaction(
        &self,
        session: &Session,
        profile: &ModelProfile,
    ) -> Result<Option<(CompactionReason, Option<MessageId>)>, KernelError>
    {
        session.transcript.inspect_history(|history| {
            let latest_compaction_started_at =
                history.iter().rev().find_map(|message| {
                    matches!(
                        message.content,
                        MessageContent::CompactionSummary { .. }
                    )
                    .then_some(message.timing.started_at_ms)
                });
            // Retained-tail messages are ordered after the synthetic summary
            // but retain their original timing, so stale provider metadata
            // must not trigger another recovery after the same compaction.
            let latest_assistant = history.iter().rev().find_map(|message| {
                if let MessageContent::Assistant { metadata, .. } =
                    &message.content
                    && latest_compaction_started_at.is_none_or(|boundary| {
                        message.timing.ended_at_ms > boundary
                    })
                {
                    Some((message, metadata))
                } else {
                    None
                }
            });
            let policy = self.compaction_policy.for_profile(profile);
            latest_assistant
                .and_then(|(message, metadata)| {
                    policy.overflow_recovery(metadata, profile).map(
                        |recovery| {
                            let excluded = (recovery
                                == OverflowRecovery::CompactAndRetry)
                                .then(|| message.identity.message_id.clone());
                            (CompactionReason::Overflow, excluded)
                        },
                    )
                })
                .or_else(|| {
                    self.exceeds_compaction_threshold(history, profile)
                        .then_some((CompactionReason::Threshold, None))
                })
        })
    }

    /// Reports whether `history` crosses the compaction threshold of the
    /// model `profile` describes, using that model's resolved budgets.
    pub(super) fn exceeds_compaction_threshold(
        &self,
        history: &[AgentMessage],
        profile: &ModelProfile,
    ) -> bool {
        ContextUsageEstimate::from_history(history).should_compact(
            profile.context_tokens,
            self.compaction_policy.for_profile(profile),
        )
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
        let cancellation = session.execution.install_cancellation(None)?;
        // A standalone compaction is a top-level Run with a trace of its own.
        let identity = self.new_compaction_identity()?;
        self.new_trace_span(&identity.run_id)?
            .instrument(Box::pin(
                self.compact_with_identity(
                    CompactionExecution::builder()
                        .session_id(session_id)
                        .session(&session)
                        .sink(sink)
                        .reason(reason)
                        .cancellation(&cancellation)
                        .build(),
                    identity,
                ),
            ))
            .await
    }

    /// Performs an automatic compaction inside the caller's Run, under the caller-owned run lock.
    ///
    /// The compaction mints a Run of its own, so it opens a nested span that
    /// inherits the caller's trace. Returns `None` when there is nothing to
    /// compact: usage can exceed the threshold through fixed overhead or a
    /// recent window that is kept whole, and failing then would fail every
    /// later prompt too, so the caller continues uncompacted.
    pub(super) async fn perform_compaction(
        &self,
        execution: CompactionExecution<'_>,
    ) -> Result<Option<CompactionResult>, KernelError> {
        let identity = self.new_compaction_identity()?;
        // The compaction body is a large future; keep it off every caller's stack frame.
        match RunSpan::nested(&identity.run_id)
            .instrument(Box::pin(
                self.compact_with_identity(execution, identity),
            ))
            .await
        {
            Ok(result) => Ok(Some(result)),
            // The guard already logged the rejection.
            Err(KernelError::NoContextToCompact) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Mints the Run, Turn, and start time of one compaction operation.
    fn new_compaction_identity(
        &self,
    ) -> Result<CompactionIdentity, KernelError> {
        Ok(CompactionIdentity::new(
            self.new_run_id()?,
            self.new_turn_id()?,
            self.clock.now(),
        ))
    }

    /// Generates and persists one compaction under an already resolved identity.
    ///
    /// The caller owns the span: standalone compaction opens a top-level Run,
    /// `/compact` runs inside its slash command's Run, and automatic compaction
    /// goes through `perform_compaction`.
    pub(super) async fn compact_with_identity(
        &self,
        execution: CompactionExecution<'_>,
        identity: CompactionIdentity,
    ) -> Result<CompactionResult, KernelError> {
        let CompactionExecution {
            session_id,
            session,
            sink,
            reason,
            cancellation,
            excluded_message_id,
            instruction,
        } = execution;
        let mut compacted_history = session.transcript.history()?;
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
        // The model that will summarize decides the budgets.
        let policy = self
            .compaction_policy
            .for_profile(session.model.active()?.profile());
        let preparation =
            CompactionPreparation::from_history(&compacted_history, policy);
        let CompactionIdentity {
            run_id,
            turn_id,
            started_at_ms,
        } = identity;
        // Direct command records are intentionally model-invisible. Rejecting
        // before lifecycle hooks and records prevents a false compaction run.
        if !preparation
            .summarized
            .iter()
            .chain(&preparation.turn_prefix)
            .any(|message| message.estimated_tokens() > 0)
        {
            // Automatic checks can meet this every Turn of a long Tool loop,
            // so only a user-requested compaction warns.
            if reason == CompactionReason::Manual {
                tracing::warn!(
                    "rejected Compaction for session {} with reason {:?}: there is no model context to compact",
                    session_id,
                    reason
                );
                // Like pi, only a requested compaction reports this failure;
                // automatic checks skip silently without building a context.
                let extension_context = self.extension_context(
                    session,
                    Some(&run_id),
                    Some(&turn_id),
                )?;
                session
                    .extensions
                    .runtime_ref()
                    .emit_session_compact_failed(
                        &protocol::SessionCompactFailedEvent::builder()
                            .reason(reason)
                            .error_message(Some(
                                KernelError::NoContextToCompact.to_string(),
                            ))
                            .build(),
                        &extension_context,
                    )
                    .await;
            } else {
                tracing::debug!(
                    "skipped Compaction for session {} with reason {:?}: there is no model context to compact",
                    session_id,
                    reason
                );
            }
            return Err(KernelError::NoContextToCompact);
        }
        let entry_id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let will_retry = reason == CompactionReason::Overflow
            && excluded_message_id.is_some();
        // Only the start line names the Session; the Run span carries it onward.
        tracing::info!(
            "started Compaction Turn {} for session {} with reason {:?}; will retry {}",
            turn_id,
            session_id,
            reason,
            will_retry
        );
        let extension_context =
            self.extension_context(session, Some(&run_id), Some(&turn_id))?;
        let extension_result = session
            .extensions
            .runtime_ref()
            .emit_session_before_compact(
                &protocol::SessionBeforeCompactEvent::builder()
                    .preparation((&preparation).into())
                    .custom_instructions(instruction.clone())
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
            tracing::warn!(
                "cancelled Compaction Turn {} with reason {:?} by extension",
                turn_id,
                reason
            );
            session
                .extensions
                .runtime_ref()
                .emit_session_compact_failed(
                    &protocol::SessionCompactFailedEvent::builder()
                        .reason(reason)
                        .aborted(true)
                        .will_retry(will_retry)
                        .build(),
                    &extension_context,
                )
                .await;
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
        let source_leaf_id = session.transcript.leaf_id()?;
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
                policy: _,
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
                                    .protocol(SummaryProtocol::TurnPrefix {
                                        custom_instructions: instruction
                                            .as_deref(),
                                    })
                                    .build(),
                            )
                            .await?;
                        let history_text = history_summary
                            .as_ref()
                            .map_or("No prior history.", |summary| {
                                summary.text.as_str()
                            });
                        let usage = history_summary.as_ref().map_or_else(
                            || prefix_summary.usage,
                            |history| {
                                {
                                    // Upstream owns partial-counter and cost accumulation.
                                    let mut usage = history.usage;
                                    usage += &prefix_summary.usage;
                                    usage
                                }
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
                                .usage(generated.usage)
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
            let summary = summary + &file_operations.summary_suffix();
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
                .usage(summary_usage)
                .details(Some(details.clone()))
                .build();
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
            // Commit the durable boundary and its in-memory context together so
            // readers cannot observe a compaction split across two locks.
            session.transcript.commit_compaction(
                NewEntry {
                    id: entry_id.clone(),
                    kind: EntryKind::Compaction,
                    payload: serde_json::to_value(&data)?,
                },
                context,
            )?;
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
                tracing::error!(
                    "failed Compaction Turn {} with reason {:?}: {}",
                    turn_id,
                    reason,
                    error
                );
                if let Err(terminal_error) = lifecycle.fail(&error).await {
                    tracing::error!(
                        "failed to settle compaction after execution failure {}: {}",
                        error,
                        terminal_error
                    );
                }
                let aborted =
                    matches!(error, KernelError::Model(ModelError::Cancelled));
                session
                    .extensions
                    .runtime_ref()
                    .emit_session_compact_failed(
                        &protocol::SessionCompactFailedEvent::builder()
                            .reason(reason)
                            .error_message(
                                (!aborted).then(|| error.to_string()),
                            )
                            .aborted(aborted)
                            .will_retry(will_retry)
                            .from_extension(from_extension)
                            .build(),
                        &extension_context,
                    )
                    .await;
                return Err(error);
            }
        };
        let terminal_result = lifecycle.complete(&result).await;
        session
            .extensions
            .runtime_ref()
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
        match terminal_result {
            Ok(()) => {
                tracing::info!(
                    "completed Compaction Turn {} with reason {:?} in {} ms; will retry {}",
                    turn_id,
                    reason,
                    result
                        .ended_at_ms
                        .get()
                        .saturating_sub(result.started_at_ms.get()),
                    will_retry
                );
                Ok(result)
            }
            Err(error) => {
                tracing::error!(
                    "failed to settle Compaction Turn {} with reason {:?}: {}",
                    turn_id,
                    reason,
                    error
                );
                Err(error)
            }
        }
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
        // Captured before the protocol is consumed so a truncation names its request.
        let failure_label = protocol.failure_label();
        // Every summary is one serialized User prompt under a dedicated
        // System prompt, so the model never continues the conversation.
        let (previous_summary, custom_instructions, template) = match protocol {
            SummaryProtocol::Compaction {
                previous_summary,
                custom_instructions,
            } => (
                previous_summary,
                custom_instructions,
                SummaryTemplate::Checkpoint,
            ),
            SummaryProtocol::TurnPrefix {
                custom_instructions,
            } => (None, custom_instructions, SummaryTemplate::TurnPrefix),
            SummaryProtocol::Branch {
                custom_instructions,
                replace_instructions,
            } => (
                None,
                custom_instructions,
                SummaryTemplate::Branch {
                    replace_instructions,
                },
            ),
        };
        let user_prompt = SummaryPrompt {
            messages: &messages,
            previous_summary,
            custom_instructions,
            template,
        }
        .render();
        let system_message = AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id()?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::System {
                blocks: vec![ContentBlock::Text {
                    text: SUMMARIZATION_SYSTEM_PROMPT.to_string(),
                }],
            },
        };
        let user_message = AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id()?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::User {
                blocks: vec![ContentBlock::Text { text: user_prompt }],
            },
        };
        let request_messages = vec![system_message, user_message];
        let completion_middleware =
            self.completion_middleware(session, run_id, turn_id)?;
        // Compaction uses one immutable model and thinking-level snapshot,
        // just like a normal Turn.
        let (model, thinking_level) = session.model.snapshot()?;
        model.preflight().await?;
        // The provider would otherwise fill in the Turn request budget, which
        // the reserve bounds; summaries size output from the request instead.
        let input_tokens =
            request_messages.iter().fold(0_u64, |total, message| {
                total.saturating_add(message.estimated_tokens())
            });
        let max_tokens = summary_max_tokens(model.profile(), input_tokens);
        let mut request = ModelRequest {
            messages: request_messages,
            tools: Vec::new(),
            // Summaries reason at the Session's level and set no cache option:
            // a one-off prompt is not reused.
            options: ModelRequestOptions::builder()
                .max_tokens(Some(max_tokens))
                .thinking_level(thinking_level)
                .build(),
        };
        if request.adapt_input(model.profile()) {
            tracing::debug!(
                "replaced unsupported images before calling compaction model {}/{}",
                model.profile().provider_id,
                model.profile().model_id
            );
        }
        let mut retry_state = RetryState::new(
            &self.retry_policy,
            self.provider_retry_delay_limit_ms,
        );
        loop {
            let result: Result<GeneratedSummary, ModelError> = async {
                let mut stream = model
                    .stream_with_middleware(
                        request.clone(),
                        cancellation.clone(),
                        Some(completion_middleware.clone()),
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
                let final_result = final_result.ok_or_else(|| {
                    ModelError::Protocol(
                        "compaction model returned no terminal usage"
                            .to_string(),
                    )
                })?;
                // Summary calls use the same authoritative final text as ordinary turns.
                let summary = final_result
                    .model_message
                    .as_ref()
                    .map_or(summary, |message| message.text());
                let summary = summary.trim().to_string();
                if summary.is_empty() {
                    return Err(ModelError::Protocol(
                        "compaction summary was empty".to_string(),
                    ));
                }
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
                    // A length stop holds partial text that must never
                    // become a checkpoint, so it fails without a retry.
                    StopReason::MaxTokens => {
                        return Err(ModelError::Protocol(format!(
                            "{failure_label} failed: generation hit the token cap and the summary is incomplete"
                        )));
                    }
                    StopReason::EndTurn => {}
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
            let schedule = match &error {
                ModelError::Preflight(failure)
                | ModelError::Request(failure)
                | ModelError::Stream(failure) => retry_state.schedule(failure),
                _ => None,
            };
            let Some(schedule) = schedule else {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a profile with only the limits the output clamp reads.
    fn profile(context_tokens: u64, max_output_tokens: u64) -> ModelProfile {
        ModelProfile::builder()
            .provider_id("p".to_string())
            .model_id("m".to_string())
            .display_name("m".to_string())
            .context_tokens(context_tokens)
            .max_output_tokens(max_output_tokens)
            .build()
    }

    /// Summary output follows pi's context clamp instead of a reserve ratio.
    #[test]
    fn summary_max_tokens_follows_pi_context_clamp() {
        assert_eq!(summary_max_tokens(&profile(100_000, 8_000), 1_000), 8_000);
        assert_eq!(
            summary_max_tokens(&profile(200_000, 64_000), 150_000),
            32_768
        );
        assert_eq!(
            summary_max_tokens(&profile(1_000_000, 128_000), 100_000),
            128_000
        );
        // An input that already fills the window still requests one token
        // instead of underflowing.
        assert_eq!(summary_max_tokens(&profile(32_000, 8_000), 30_000), 1);
    }
}
