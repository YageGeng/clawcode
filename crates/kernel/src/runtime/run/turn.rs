//! The Turn loop of one Agent Run, split into phases that share borrowed Run
//! context. Each phase keeps the event order and cancellation checks of the
//! original single loop; only the boundaries between them are new.

use std::sync::Arc;
use std::time::Duration;

use ::extension::{ExtensionContext, MessageUpdatePoint};
use ::tools::ToolRegistry;
use futures::StreamExt;
use protocol::{
    AgentEventPayload, AgentMessage, AgentOutcome, CompactionReason,
    ContentBlock, ExtensionMessageDraft, MessageContent, MessageId,
    ModelRequest, ModelStreamEvent, RunId, RunResult, SessionId, StopReason,
    ThinkingLevel, TimestampMs, ToolCall, ToolResult, TraceId, TurnId,
    TurnIdentity, TurnOutcome, TurnRecord, TurnTiming,
};
use store::RecordKind;
use tokio_util::sync::CancellationToken;

use super::attempt::{
    AssistantAttempt, AssistantAttemptResult, AssistantAttemptSettlement,
};
use super::insertion::SubmissionAnswer;
use super::queue::{ClaimedItem, QueueCancellationReason};
use super::retry::RetryState;
use super::settlement::{RunCompletion, RunSettlement};
use crate::runtime::compaction::{
    CompactionExecution, CompactionPolicyExt, ContextUsageEstimate,
    OverflowRecovery,
};
use crate::runtime::plan::PlanModeGate;
use crate::runtime::session::rename::SessionRename;
use crate::runtime::tools::{ToolBatch, ToolBatchResult};
use crate::runtime::{
    EventEmitter, InsertionNotice, Kernel, KernelError, Session,
};
use crate::{Model, ModelError};

use super::RetainedInput;

/// Borrowed context shared by every Turn of one Agent Run.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct RunScope<'a> {
    kernel: &'a Kernel,
    session_id: &'a SessionId,
    session: &'a Arc<Session>,
    run_id: &'a RunId,
    trace_id: &'a TraceId,
    emitter: &'a EventEmitter,
    cancellation: &'a CancellationToken,
    /// Origin reported to input hooks and to retained-input settlement.
    input_source: protocol::InputSource,
    /// Client acceptance report; absent for internal input.
    #[builder(default)]
    insertion: Option<&'a InsertionNotice>,
}

/// Continuation state carried across model, Tool, retry, overflow-recovery,
/// and queued-message Turns of one Run.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct TurnLoop<'a> {
    #[builder(default)]
    produced_messages: Vec<AgentMessage>,
    #[builder(default)]
    turns: Vec<TurnRecord>,
    /// The Run's own input, inserted by its first Turn.
    #[builder(default, setter(strip_option))]
    next_initial_input: Option<MessageContent>,
    #[builder(default)]
    next_queued_item: Option<ClaimedItem>,
    /// Identity of the next Turn when it was allocated ahead of time.
    #[builder(default, setter(strip_option))]
    next_turn_id: Option<TurnId>,
    retry_state: RetryState<'a>,
    #[builder(default)]
    overflow_recovery_attempted: bool,
    /// System prompt replacement chosen by `before_agent_start` hooks.
    #[builder(default)]
    system_prompt_override: Option<String>,
    /// Messages `before_agent_start` hooks add ahead of the first request.
    #[builder(default)]
    before_agent_messages: Option<Vec<ExtensionMessageDraft>>,
    /// Internal input with no client to return it to: a cancellation before
    /// insertion persists this copy instead of dropping it.
    #[builder(default)]
    retained_input: Option<MessageContent>,
    /// Whether a client prompt reached history; settlement reads it on failure.
    #[builder(default)]
    user_message_inserted: bool,
    /// Usage of every settled attempt; settlement reads it on any outcome.
    #[builder(default)]
    usage: protocol::ModelUsage,
}

impl TurnLoop<'_> {
    /// Hands the produced messages and Turns to the Run's terminal completion.
    fn finish(
        &mut self,
        run_id: &RunId,
        turn_id: TurnId,
        outcome: AgentOutcome,
        auto_compaction_reason: Option<CompactionReason>,
    ) -> RunCompletion {
        RunCompletion::builder()
            .result(RunResult {
                run_id: run_id.clone(),
                messages: std::mem::take(&mut self.produced_messages),
                turns: std::mem::take(&mut self.turns),
            })
            .turn_id(turn_id)
            .outcome(outcome)
            .auto_compaction_reason(auto_compaction_reason)
            .build()
    }
}

/// One Turn's identity, frozen model snapshot, and extension context.
#[derive(typed_builder::TypedBuilder)]
struct TurnContext {
    turn_id: TurnId,
    started_at: TimestampMs,
    /// Snapshotted once so host changes made during the Turn apply only to the next.
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    extension_context: ExtensionContext,
}

/// One settled Assistant attempt and what it asks the Turn to do next.
#[derive(typed_builder::TypedBuilder)]
struct SettledAssistant {
    assistant_id: MessageId,
    /// Usage after `message_end` hooks, reported once the message is durable.
    usage: protocol::ModelUsage,
    #[builder(default)]
    failure: Option<protocol::ModelFailure>,
    cancelled: bool,
    #[builder(default)]
    overflow_recovery: Option<OverflowRecovery>,
    stop_reason: StopReason,
    tool_calls: Vec<ToolCall>,
}

/// Tool results of one Turn and whether a Tool ended the Run's continuation.
struct ToolTurn {
    results: Vec<ToolResult>,
    terminated: bool,
}

impl RunScope<'_> {
    /// Runs Agent hooks and pre-prompt compaction, then Turns until one settles the Run.
    pub(in crate::runtime) async fn execute(
        &self,
        turn_loop: &mut TurnLoop<'_>,
        prompt: String,
        extension_context: &ExtensionContext,
    ) -> Result<RunCompletion, KernelError> {
        self.start_agent(turn_loop, prompt, extension_context)
            .await?;
        loop {
            if let Some(completion) = self.run_turn(turn_loop).await? {
                return Ok(completion);
            }
        }
    }

    /// Freezes Run-scoped prompt inputs, runs Agent start hooks, and compacts
    /// the previous context before the new user message enters history.
    async fn start_agent(
        &self,
        turn_loop: &mut TurnLoop<'_>,
        prompt: String,
        extension_context: &ExtensionContext,
    ) -> Result<(), KernelError> {
        let kernel = self.kernel;
        // Freeze Run-scoped tools, Skills, and System Prompt contributions
        // before invoking Agent lifecycle hooks.
        let initial_tools = self.session.tools.snapshot()?.active_registry()?;
        let initial_skills = self
            .session
            .skill_catalog()?
            .map_or_else(Vec::new, |catalog| catalog.descriptors());
        let initial_system_prompt = self.session.build_system_prompt(
            &initial_tools,
            kernel.include_skill_instructions,
        )?;
        let before_agent = self
            .session
            .extensions
            .runtime_ref()
            .emit_before_agent_start(
                protocol::BeforeAgentStartEvent {
                    prompt,
                    system_prompt: initial_system_prompt.text,
                    system_prompt_options: initial_system_prompt.options,
                    skills: initial_skills
                        .into_iter()
                        .map(|skill| skill.name)
                        .collect(),
                },
                extension_context,
            )
            .await;
        turn_loop.system_prompt_override = before_agent.system_prompt;
        turn_loop.before_agent_messages = Some(before_agent.messages);
        self.session
            .extensions
            .runtime_ref()
            .emit_agent_start(&protocol::AgentStartEvent, extension_context)
            .await;
        let initial_model = self.session.model.active()?;

        // Compact the previous active context before the new user message
        // enters durable history.
        let pre_prompt_compaction = kernel
            .pre_prompt_compaction(self.session, initial_model.profile())?;
        if let Some((reason, excluded_message_id)) = pre_prompt_compaction {
            // Pi checks the previous Assistant before persisting a newly
            // submitted user message, preventing an avoidable overflow call.
            kernel
                .perform_compaction(
                    CompactionExecution::builder()
                        .session_id(self.session_id)
                        .session(self.session)
                        .sink(Arc::clone(&self.emitter.sink))
                        .reason(reason)
                        .cancellation(self.cancellation)
                        .excluded_message_id(excluded_message_id.as_ref())
                        .build(),
                )
                .await?;
        }
        Ok(())
    }

    /// Runs one complete Turn and returns the Run's completion when it ends.
    async fn run_turn(
        &self,
        turn_loop: &mut TurnLoop<'_>,
    ) -> Result<Option<RunCompletion>, KernelError> {
        // Taken first so a claim the limit rejects drops here and returns to
        // the queue before settlement closes and withdraws it.
        let queued_item = turn_loop.next_queued_item.take();
        // Check the policy before allocating or emitting the next Turn.
        if let config::TurnLimit::Limited { turns: max_turns } =
            self.kernel.max_turns
            && turn_loop.turns.len() >= max_turns.get()
        {
            return Err(KernelError::TurnLimit(max_turns.get()));
        }
        let turn = self.begin_turn(turn_loop, queued_item.as_ref()).await?;
        self.insert_user_message(&turn, turn_loop, queued_item)
            .await?;
        let (request, tools, gate) =
            self.model_request(&turn, turn_loop).await?;
        let (assistant_id, attempt) =
            self.stream_assistant(&turn, request).await?;
        let (assistant, mut settled) = self
            .settle_assistant(
                &turn,
                assistant_id,
                attempt,
                &mut turn_loop.usage,
            )
            .await?;
        self.publish_assistant(&turn, turn_loop, assistant, settled.usage)
            .await?;
        let has_tool_calls = !settled.tool_calls.is_empty();
        let tool_turn = self
            .run_tools(&turn, turn_loop, &mut settled, tools, &gate)
            .await?;
        self.record_turn(&turn, turn_loop, &settled, tool_turn.results)
            .await?;
        self.next_action(
            &turn,
            turn_loop,
            settled,
            !tool_turn.terminated && has_tool_calls,
        )
        .await
    }

    /// Allocates the Turn, snapshots its model, and announces its start.
    async fn begin_turn(
        &self,
        turn_loop: &mut TurnLoop<'_>,
        queued_item: Option<&ClaimedItem>,
    ) -> Result<TurnContext, KernelError> {
        let kernel = self.kernel;
        let turn_id = queued_item
            .map(|item| item.item().queued.message.identity.turn_id.clone())
            .or_else(|| turn_loop.next_turn_id.take())
            .map_or_else(|| kernel.new_turn_id(), Ok)?;
        let started_at = kernel.clock.now();
        tracing::info!("started Turn {}", turn_id);
        // Model selection is snapshotted once so host changes made during
        // this Turn apply only to the next Turn.
        let (model, thinking_level) = self.session.model.snapshot()?;
        model.preflight().await?;
        self.session
            .execution
            .set_active_turn(self.run_id, turn_id.clone())?;
        self.emitter
            .emit(
                turn_id.clone(),
                AgentEventPayload::TurnStart {
                    run_id: self.run_id.clone(),
                },
            )
            .await?;
        let extension_context = kernel.extension_context(
            self.session,
            Some(self.run_id),
            Some(&turn_id),
        )?;
        self.session
            .extensions
            .runtime_ref()
            .emit_turn_start(
                &protocol::TurnStartEvent {
                    turn_index: turn_loop.turns.len(),
                    timestamp_ms: started_at,
                },
                &extension_context,
            )
            .await;
        Ok(TurnContext::builder()
            .turn_id(turn_id)
            .started_at(started_at)
            .model(model)
            .thinking_level(thinking_level)
            .extension_context(extension_context)
            .build())
    }

    /// Materializes and persists the user-side message of the Turn: the Run's
    /// initial input or one queued follow-up.
    async fn insert_user_message(
        &self,
        turn: &TurnContext,
        turn_loop: &mut TurnLoop<'_>,
        mut queued_item: Option<ClaimedItem>,
    ) -> Result<(), KernelError> {
        let kernel = self.kernel;
        let turn_id = &turn.turn_id;
        let user_message = match queued_item.as_ref() {
            Some(item) => Some(item.item().queued.message.clone()),
            None => turn_loop
                .next_initial_input
                .take()
                .map(|content| kernel.message_now(turn_id, content))
                .transpose()?,
        };
        // A client prompt cancelled during hooks, compaction, or
        // preflight is rejected before its message enters history.
        if self.insertion.is_some()
            && queued_item.is_none()
            && user_message.is_some()
            && self.cancellation.is_cancelled()
        {
            return Err(KernelError::PromptCancelled);
        }
        let Some(message) = user_message else {
            return Ok(());
        };
        let timestamp = message.timing.timestamp_ms;
        self.emitter
            .emit_at(
                turn_id.clone(),
                timestamp,
                AgentEventPayload::MessageStart {
                    message_id: message.identity.message_id.clone(),
                },
            )
            .await?;
        self.session
            .extensions
            .runtime_ref()
            .emit_message_start(
                &protocol::MessageStartEvent {
                    message: message.clone(),
                },
                &turn.extension_context,
            )
            .await;
        let message = self
            .session
            .extensions
            .runtime_ref()
            .emit_message_end(
                protocol::MessageEndEvent { message },
                &turn.extension_context,
            )
            .await;
        if queued_item.is_some() && self.cancellation.is_cancelled() {
            return Err(KernelError::PromptCancelled);
        }
        match queued_item.take() {
            Some(claimed) => {
                let item = claimed.item();
                let record = kernel.queue_cancellation_record(
                    self.session,
                    Some(self.run_id),
                    item.queued.queue_id.clone(),
                    QueueCancellationReason::Consumed,
                )?;
                self.session.consume_claimed(claimed, &message, record)?;
            }
            None => {
                // Message observers may await after the earlier cancellation check.
                if self.insertion.is_some() && self.cancellation.is_cancelled()
                {
                    tracing::info!(
                        "cancelled prompt after message hooks before insertion"
                    );
                    return Err(KernelError::PromptCancelled);
                }
                kernel.persist_message(self.session, &message)?;
                turn_loop.user_message_inserted = true;
                if let Some(notice) = self.insertion {
                    notice.inserted(&message.identity.message_id);
                }
            }
        }
        // The default title takes the rename path, so it is ordered with
        // client titles and reaches `session_info_changed` observers too.
        if let Some(title) = message.default_title() {
            SessionRename::builder()
                .kernel(kernel)
                .session_id(self.session_id)
                .session(self.session)
                .build()
                .default_on(title, self.emitter, turn_id.clone())
                .await?;
        }
        self.emitter
            .emit_at(
                turn_id.clone(),
                timestamp,
                AgentEventPayload::MessageEnd {
                    message: message.clone(),
                },
            )
            .await?;
        turn_loop.produced_messages.push(message);
        Ok(())
    }

    /// Builds one provider request from durable history and an immutable Tool
    /// registry snapshot for this Turn.
    async fn model_request(
        &self,
        turn: &TurnContext,
        turn_loop: &mut TurnLoop<'_>,
    ) -> Result<(ModelRequest, Arc<ToolRegistry>, PlanModeGate), KernelError>
    {
        let kernel = self.kernel;
        let turn_id = &turn.turn_id;
        let mut history = self.session.transcript.history()?;
        // Failed Assistant attempts remain replayable in Store but are
        // excluded from the next active provider context, matching pi.
        history.retain(|message| {
            !matches!(
                &message.content,
                MessageContent::Assistant { metadata, .. }
                    if metadata.error.is_some()
            )
        });
        // One immutable registry snapshot defines the complete Turn.
        let snapshot = self.session.tools.snapshot()?;
        let turn_tools = Arc::new(snapshot.active_registry()?);
        // The mode is read with the snapshot, so a switch applies from the next Turn.
        let gate = PlanModeGate::new(
            self.session.plan.mode()?,
            snapshot.kernel_owned.clone(),
        );
        let mut system_prompt = kernel.system_prompt_message(
            self.session,
            turn_id.clone(),
            turn.started_at,
            &turn_tools,
        )?;
        if let Some(replacement) = turn_loop.system_prompt_override.as_ref() {
            system_prompt.content = MessageContent::System {
                blocks: vec![ContentBlock::Text {
                    text: replacement.clone(),
                }],
            };
        }
        if let Some(messages) = turn_loop.before_agent_messages.take() {
            for draft in messages {
                let message =
                    kernel.materialize_extension_message(turn_id, draft)?;
                self.session
                    .extensions
                    .runtime_ref()
                    .emit_message_start(
                        &protocol::MessageStartEvent {
                            message: message.clone(),
                        },
                        &turn.extension_context,
                    )
                    .await;
                let message = self
                    .session
                    .extensions
                    .runtime_ref()
                    .emit_message_end(
                        protocol::MessageEndEvent { message },
                        &turn.extension_context,
                    )
                    .await;
                kernel.persist_message(self.session, &message)?;
                self.emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::MessageEnd {
                            message: message.clone(),
                        },
                    )
                    .await?;
                history.push(message.clone());
                turn_loop.produced_messages.push(message);
            }
        }
        history.insert(0, system_prompt);
        // Context hooks take the request by value and hand it back, so the
        // Turn's history is not copied to give extensions a view of it.
        let request = ModelRequest {
            messages: history,
            tools: turn_tools.definitions(),
            // Only providers configured for prompt caching get a retention, and
            // the Session id routes each of their Turns to the server that holds
            // the conversation's cached prefix.
            options: protocol::ModelRequestOptions::builder()
                .thinking_level(turn.thinking_level)
                .prompt_cache(turn.model.profile().prompt_cache.map(
                    |retention| protocol::PromptCache {
                        retention,
                        key: self.session_id.to_string(),
                    },
                ))
                .build(),
        };
        let mut request = self
            .session
            .extensions
            .runtime_ref()
            .emit_context(
                protocol::ContextEvent { request },
                &turn.extension_context,
            )
            .await;
        if request.adapt_input(turn.model.profile()) {
            tracing::debug!(
                "replaced unsupported images before calling model {}/{}",
                turn.model.profile().provider_id,
                turn.model.profile().model_id
            );
        }
        Ok((request, turn_tools, gate))
    }

    /// Streams one Assistant attempt, publishing incremental updates while
    /// retaining a complete terminal snapshot for settlement.
    async fn stream_assistant(
        &self,
        turn: &TurnContext,
        request: ModelRequest,
    ) -> Result<(MessageId, AssistantAttemptResult), KernelError> {
        let kernel = self.kernel;
        let turn_id = &turn.turn_id;
        let extension_context = &turn.extension_context;
        let assistant_id = kernel.message_id()?;
        let assistant_timestamp = kernel.clock.now();
        let mut attempt = AssistantAttempt::new(
            assistant_id.clone(),
            turn_id.clone(),
            turn.model.profile().clone(),
            assistant_timestamp,
        );
        self.emitter
            .emit_at(
                turn_id.clone(),
                assistant_timestamp,
                AgentEventPayload::MessageStart {
                    message_id: assistant_id.clone(),
                },
            )
            .await?;
        self.session
            .extensions
            .runtime_ref()
            .emit_message_start(
                &protocol::MessageStartEvent {
                    message: attempt.snapshot(assistant_timestamp)?,
                },
                extension_context,
            )
            .await;
        // A snapshot copies the whole accumulated message on every delta, so
        // only pay for it when a handler reads it. Registries are frozen for
        // the Session's lifetime, so one check holds for the whole stream.
        let observes_updates = self
            .session
            .extensions
            .runtime_ref()
            .has_handlers::<MessageUpdatePoint>();

        // Provider cancellation, stream closure, and stream errors all
        // converge into one typed Assistant settlement.
        let settlement = match turn
            .model
            .stream_with_middleware(
                request,
                self.cancellation.clone(),
                Some(kernel.completion_middleware(
                    self.session,
                    self.run_id,
                    turn_id,
                )?),
            )
            .await
        {
            Ok(mut stream) => loop {
                let item = tokio::select! {
                    () = self.cancellation.cancelled() => {
                        break AssistantAttemptSettlement::Cancelled {
                            settled_at_ms: kernel.clock.now(),
                        };
                    }
                    item = stream.next() => item,
                };
                let Some(item) = item else {
                    break AssistantAttemptSettlement::Completed {
                        settled_at_ms: kernel.clock.now(),
                    };
                };
                let item = match item {
                    Ok(item) => item,
                    Err(error) => {
                        break AssistantAttemptSettlement::Failed {
                            error,
                            settled_at_ms: kernel.clock.now(),
                        };
                    }
                };
                let timestamp = kernel.clock.now();
                if let Err(error) = attempt.apply(&item, timestamp) {
                    break AssistantAttemptSettlement::Failed {
                        error,
                        settled_at_ms: kernel.clock.now(),
                    };
                }
                let update = match item {
                    ModelStreamEvent::TextDelta(delta) => {
                        self.emitter
                            .emit_at(
                                turn_id.clone(),
                                timestamp,
                                AgentEventPayload::MessageTextDelta {
                                    message_id: assistant_id.clone(),
                                    delta: delta.clone(),
                                },
                            )
                            .await?;
                        protocol::MessageUpdate::Text { delta }
                    }
                    ModelStreamEvent::ReasoningDelta(delta) => {
                        self.emitter
                            .emit_at(
                                turn_id.clone(),
                                timestamp,
                                AgentEventPayload::MessageReasoningDelta {
                                    message_id: assistant_id.clone(),
                                    delta: delta.clone(),
                                },
                            )
                            .await?;
                        protocol::MessageUpdate::Reasoning { delta }
                    }
                    ModelStreamEvent::ToolCall(call) => {
                        protocol::MessageUpdate::ToolCall { call }
                    }
                    ModelStreamEvent::Finished(_final) => continue,
                };
                if observes_updates {
                    self.session
                        .extensions
                        .runtime_ref()
                        .emit_message_update(
                            &protocol::MessageUpdateEvent {
                                message: attempt.snapshot(timestamp)?,
                                update,
                            },
                            extension_context,
                        )
                        .await;
                }
            },
            Err(ModelError::Cancelled) => {
                AssistantAttemptSettlement::Cancelled {
                    settled_at_ms: kernel.clock.now(),
                }
            }
            Err(error) => AssistantAttemptSettlement::Failed {
                error,
                settled_at_ms: kernel.clock.now(),
            },
        };
        Ok((assistant_id, attempt.settle(settlement)?))
    }

    /// Runs `message_end` hooks on a settled attempt, then decides its failure,
    /// cancellation, and Tool calls from that final snapshot.
    async fn settle_assistant(
        &self,
        turn: &TurnContext,
        assistant_id: MessageId,
        attempt: AssistantAttemptResult,
        run_usage: &mut protocol::ModelUsage,
    ) -> Result<(AgentMessage, SettledAssistant), KernelError> {
        let kernel = self.kernel;
        let extension_context = &turn.extension_context;
        let (assistant, mut failure, mut cancelled) = match attempt {
            AssistantAttemptResult::Completed { message } => {
                (message, None, false)
            }
            AssistantAttemptResult::Failed { message, failure } => {
                (message, Some(failure), false)
            }
            AssistantAttemptResult::Cancelled { message } => {
                (message, None, true)
            }
        };
        // Count the settled attempt before validation or persistence can fail.
        if let MessageContent::Assistant { metadata, .. } = &assistant.content {
            *run_usage += &metadata.usage;
        }
        // Native provenance does not change the assistant role or its lifecycle.
        let mut assistant = self
            .session
            .extensions
            .runtime_ref()
            .emit_message_end(
                protocol::MessageEndEvent { message: assistant },
                extension_context,
            )
            .await;
        let MessageContent::Assistant {
            blocks,
            metadata,
            model_message,
        } = &mut assistant.content
        else {
            // ExtensionRuntime keeps the role, but that guarantee lives in another
            // crate, so a broken one must fail this Run instead of panicking.
            tracing::error!(
                "Assistant message {} lost its role before settlement",
                assistant_id
            );
            return Err(KernelError::Protocol(
                "settled Assistant message lost its role".to_string(),
            ));
        };
        // Hooks may stop a successful turn, but cannot revive a failed/cancelled provider attempt.
        cancelled |= metadata.stop_reason == StopReason::Cancelled;
        if cancelled {
            metadata.stop_reason = StopReason::Cancelled;
        } else if let Some(failure) = &failure {
            metadata.stop_reason = StopReason::Error;
            metadata.error = Some(failure.summary.clone());
            metadata.context_overflow = failure.context_overflow;
        } else if matches!(
            metadata.stop_reason,
            StopReason::Error | StopReason::Refusal
        ) {
            failure = Some(
                protocol::ModelFailure::builder()
                    .summary(metadata.error.clone().unwrap_or_else(|| {
                        "MessageEnd ended the turn with an error".into()
                    }))
                    .retry_disposition(
                        protocol::ModelRetryDisposition::NonRetryable,
                    )
                    .context_overflow(metadata.context_overflow)
                    .build(),
            );
        }
        // Compare and execute the same final snapshot; no pre-hook call list is retained.
        if let Some(native) = model_message.as_ref()
            && !crate::provider::AssistantProjection::from(native.as_ref())
                .matches(blocks, metadata.stop_reason)?
        {
            *model_message = None;
        }
        let tool_calls = if failure.is_none()
            && !cancelled
            && matches!(
                metadata.stop_reason,
                StopReason::ToolUse | StopReason::MaxTokens
            ) {
            blocks
                .iter()
                .filter_map(|block| {
                    let ContentBlock::ToolCall {
                        tool_call_id,
                        name,
                        arguments,
                        invalid_arguments,
                    } = block
                    else {
                        return None;
                    };
                    Some(
                        ToolCall::builder()
                            .tool_call_id(tool_call_id.clone())
                            .name(name.clone())
                            .arguments(arguments.clone())
                            .invalid_arguments(invalid_arguments.clone())
                            .build(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let settled = SettledAssistant::builder()
            .assistant_id(assistant_id)
            .usage(metadata.usage)
            .overflow_recovery(
                kernel
                    .compaction_policy
                    .overflow_recovery(metadata, turn.model.profile()),
            )
            .stop_reason(metadata.stop_reason)
            .failure(failure)
            .cancelled(cancelled)
            .tool_calls(tool_calls)
            .build();
        Ok((assistant, settled))
    }

    /// Persists the settled Assistant message and reports it with its usage.
    async fn publish_assistant(
        &self,
        turn: &TurnContext,
        turn_loop: &mut TurnLoop<'_>,
        assistant: AgentMessage,
        usage: protocol::ModelUsage,
    ) -> Result<(), KernelError> {
        self.kernel.persist_message(self.session, &assistant)?;
        turn_loop.produced_messages.push(assistant.clone());
        self.emitter
            .emit(
                turn.turn_id.clone(),
                AgentEventPayload::MessageEnd { message: assistant },
            )
            .await?;
        self.emitter
            .emit(
                turn.turn_id.clone(),
                AgentEventPayload::UsageUpdated {
                    usage,
                    context_window: turn.model.profile().context_tokens,
                },
            )
            .await
    }

    /// Executes the Turn's Tool calls, then persists their result messages.
    ///
    /// Tool execution is skipped after cancellation; otherwise all calls
    /// settle before their result messages are persisted.
    async fn run_tools(
        &self,
        turn: &TurnContext,
        turn_loop: &mut TurnLoop<'_>,
        settled: &mut SettledAssistant,
        tools: Arc<ToolRegistry>,
        gate: &PlanModeGate,
    ) -> Result<ToolTurn, KernelError> {
        let turn_id = &turn.turn_id;
        let tool_batch = if settled.cancelled {
            ToolBatchResult::default()
        } else {
            self.kernel
                .execute_tool_batch(
                    ToolBatch::builder()
                        .session_id(self.session_id)
                        .run_id(self.run_id)
                        .turn_id(turn_id)
                        .trace_id(self.trace_id)
                        .session(self.session)
                        .emitter(self.emitter)
                        .cancellation(self.cancellation)
                        .tools(tools)
                        .gate(gate)
                        .calls(std::mem::take(&mut settled.tool_calls))
                        .build(),
                )
                .await?
        };
        settled.cancelled |= tool_batch.cancelled;
        for message in tool_batch.messages {
            self.emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::MessageStart {
                        message_id: message.identity.message_id.clone(),
                    },
                )
                .await?;
            self.session
                .extensions
                .runtime_ref()
                .emit_message_start(
                    &protocol::MessageStartEvent {
                        message: message.clone(),
                    },
                    &turn.extension_context,
                )
                .await;
            let cancelled_call = matches!(
                message.content,
                MessageContent::ToolResult {
                    cancelled: true,
                    ..
                }
            );
            let mut message = self
                .session
                .extensions
                .runtime_ref()
                .emit_message_end(
                    protocol::MessageEndEvent { message },
                    &turn.extension_context,
                )
                .await;
            // A replaced result keeps the cancellation the Turn already decided.
            if let MessageContent::ToolResult { cancelled, .. } =
                &mut message.content
            {
                *cancelled |= cancelled_call;
            }
            self.kernel.persist_message(self.session, &message)?;
            turn_loop.produced_messages.push(message.clone());
            self.emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::MessageEnd { message },
                )
                .await?;
        }
        Ok(ToolTurn {
            results: tool_batch.results,
            terminated: tool_batch.terminate,
        })
    }

    /// Makes the Turn durable after its Assistant and Tool messages reached the
    /// transcript, then publishes its completion.
    async fn record_turn(
        &self,
        turn: &TurnContext,
        turn_loop: &mut TurnLoop<'_>,
        settled: &SettledAssistant,
        tool_results: Vec<ToolResult>,
    ) -> Result<(), KernelError> {
        let kernel = self.kernel;
        let turn_id = &turn.turn_id;
        let turn_ended_at = kernel.clock.now();
        let record = TurnRecord {
            identity: TurnIdentity {
                run_id: self.run_id.clone(),
                turn_id: turn_id.clone(),
            },
            timing: TurnTiming::try_from((turn.started_at, turn_ended_at))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            outcome: if settled.cancelled {
                TurnOutcome::Cancelled
            } else if let Some(failure) = &settled.failure {
                TurnOutcome::Failed {
                    message: failure.summary.clone(),
                }
            } else {
                TurnOutcome::Completed {
                    stop_reason: settled.stop_reason,
                }
            },
        };
        kernel.record_operation(
            self.session,
            self.run_id,
            RecordKind::StepAttempt,
            serde_json::to_value(&record)?,
        )?;
        // Publish Turn completion only after its transcript and recovery
        // record have reached durable storage.
        self.session.sync_store()?;
        self.emitter
            .emit(
                turn_id.clone(),
                AgentEventPayload::TurnEnd {
                    turn: record.clone(),
                },
            )
            .await?;
        self.session
            .extensions
            .runtime_ref()
            .emit_turn_end(
                &protocol::TurnEndEvent {
                    turn: record.clone(),
                    tool_results,
                },
                &turn.extension_context,
            )
            .await;
        // A settled Turn is the durable checkpoint: force the session log
        // again after TurnEnd hooks in case they persisted extension state.
        self.session.sync_store()?;
        tracing::info!(
            "settled Turn {} in {} ms with outcome {:?}",
            turn_id,
            turn_ended_at.get().saturating_sub(turn.started_at.get()),
            record.outcome
        );
        turn_loop.turns.push(record);
        Ok(())
    }

    /// Chooses the one next action of a settled Turn, in priority order:
    /// cancellation, overflow recovery, retry, failure, then continuation.
    async fn next_action(
        &self,
        turn: &TurnContext,
        turn_loop: &mut TurnLoop<'_>,
        settled: SettledAssistant,
        continue_for_tools: bool,
    ) -> Result<Option<RunCompletion>, KernelError> {
        let kernel = self.kernel;
        let turn_id = &turn.turn_id;
        if settled.cancelled {
            return Ok(Some(turn_loop.finish(
                self.run_id,
                turn_id.clone(),
                AgentOutcome::Cancelled,
                None,
            )));
        }

        // Overflow recovery compacts and retries at most once per Run. With
        // nothing to compact, a retry would overflow again, so the failure
        // settles below instead.
        if settled.overflow_recovery == Some(OverflowRecovery::CompactAndRetry)
            && !turn_loop.overflow_recovery_attempted
        {
            turn_loop.overflow_recovery_attempted = true;
            if kernel
                .perform_compaction(
                    CompactionExecution::builder()
                        .session_id(self.session_id)
                        .session(self.session)
                        .sink(Arc::clone(&self.emitter.sink))
                        .reason(CompactionReason::Overflow)
                        .cancellation(self.cancellation)
                        .excluded_message_id(Some(&settled.assistant_id))
                        .build(),
                )
                .await?
                .is_some()
            {
                turn_loop.next_turn_id = Some(kernel.new_turn_id()?);
                return Ok(None);
            }
        }

        // Retryable provider failures use cancellable backoff before a
        // newly correlated retry Turn begins.
        if let Some(retry_failure) = settled.failure.as_ref()
            && let Some(schedule) =
                turn_loop.retry_state.schedule(retry_failure)
        {
            self.emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::RetryScheduled {
                        run_id: self.run_id.clone(),
                        attempt: schedule.attempt,
                        max_attempts: schedule.max_attempts,
                        delay_ms: schedule.delay_ms,
                        error: retry_failure.summary.clone(),
                    },
                )
                .await?;
            let backoff =
                tokio::time::sleep(Duration::from_millis(schedule.delay_ms));
            tokio::pin!(backoff);
            let retry_cancelled = tokio::select! {
                () = self.cancellation.cancelled() => true,
                () = &mut backoff => false,
            };
            if retry_cancelled {
                self.emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::RetryEnd {
                            run_id: self.run_id.clone(),
                            attempt: schedule.attempt,
                            success: false,
                            final_error: Some("Retry cancelled".to_string()),
                        },
                    )
                    .await?;
                return Ok(Some(turn_loop.finish(
                    self.run_id,
                    turn_id.clone(),
                    AgentOutcome::Cancelled,
                    None,
                )));
            }

            let retry_turn_id = kernel.new_turn_id()?;
            self.emitter
                .emit(
                    retry_turn_id.clone(),
                    AgentEventPayload::RetryStart {
                        run_id: self.run_id.clone(),
                        attempt: schedule.attempt,
                        max_attempts: schedule.max_attempts,
                    },
                )
                .await?;
            turn_loop.next_turn_id = Some(retry_turn_id);
            return Ok(None);
        }

        // Publish one terminal retry outcome when a retry sequence
        // succeeds or exhausts its configured attempts.
        if settled.failure.is_none() && turn_loop.retry_state.attempts() > 0 {
            self.emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::RetryEnd {
                        run_id: self.run_id.clone(),
                        attempt: turn_loop.retry_state.attempts(),
                        success: true,
                        final_error: None,
                    },
                )
                .await?;
            turn_loop.retry_state.reset();
        } else if let Some(retry_failure) = settled.failure.as_ref()
            && turn_loop.retry_state.attempts() > 0
        {
            self.emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::RetryEnd {
                        run_id: self.run_id.clone(),
                        attempt: turn_loop.retry_state.attempts(),
                        success: false,
                        final_error: Some(retry_failure.summary.clone()),
                    },
                )
                .await?;
        }

        // A non-retryable or exhausted failure ends the Run after the
        // failed Turn has been fully settled.
        if let Some(failure) = settled.failure {
            return Ok(Some(turn_loop.finish(
                self.run_id,
                turn_id.clone(),
                AgentOutcome::Failed {
                    message: failure.summary,
                },
                None,
            )));
        }

        // Tool use and queued follow-ups are the normal reasons to
        // continue after a successful Turn.
        turn_loop.next_queued_item = self
            .session
            .execution
            .claim_next_or_close(!continue_for_tools)?;
        if turn_loop.next_queued_item.is_some() {
            kernel.publish_queue_changed(self.session_id);
        }
        if continue_for_tools || turn_loop.next_queued_item.is_some() {
            return Ok(None);
        }
        // Post-Run compaction is scheduled only after no pending
        // continuation can consume the current context.
        let should_threshold_compact =
            self.session.transcript.inspect_history(|history| {
                ContextUsageEstimate::from_history(history).should_compact(
                    turn.model.profile().context_tokens,
                    kernel.compaction_policy,
                )
            })?;
        let auto_compaction_reason = match settled.overflow_recovery {
            Some(OverflowRecovery::CompactOnly) => {
                Some(CompactionReason::Overflow)
            }
            Some(OverflowRecovery::CompactAndRetry) | None
                if should_threshold_compact =>
            {
                Some(CompactionReason::Threshold)
            }
            Some(OverflowRecovery::CompactAndRetry) | None => None,
        };
        Ok(Some(turn_loop.finish(
            self.run_id,
            turn_id.clone(),
            AgentOutcome::Succeeded,
            auto_compaction_reason,
        )))
    }
}

impl RunScope<'_> {
    /// Passes text input through Extension input hooks before server-side
    /// Skill and Template expansion; structured blocks remain unchanged.
    ///
    /// Returns `None` when a hook handled the input, which ends the Run.
    pub(in crate::runtime) async fn transform_input(
        &self,
        input: protocol::RunInput,
        input_processed: bool,
        first_turn_id: &TurnId,
        extension_context: &ExtensionContext,
    ) -> Result<Option<protocol::RunInput>, KernelError> {
        let protocol::RunInput::Text(command_input) = input else {
            return Ok(Some(input));
        };
        let result = if input_processed {
            protocol::InputResult::Continue
        } else {
            self.session
                .extensions
                .runtime_ref()
                .emit_input(
                    protocol::InputEvent {
                        text: command_input.clone(),
                        source: self.input_source,
                        streaming_behavior: None,
                    },
                    extension_context,
                )
                .await
        };
        let text = match result {
            protocol::InputResult::Continue => command_input,
            protocol::InputResult::Transform { text } => text,
            protocol::InputResult::Handled => {
                // Input handlers may persist through the Extension Host even
                // when they consume the request before an Agent Turn starts.
                if let Some(notice) = self.insertion {
                    self.kernel
                        .insert_handled(
                            self.emitter,
                            first_turn_id,
                            command_input,
                            notice,
                            self.cancellation,
                        )
                        .await?;
                } else {
                    self.session.sync_store()?;
                }
                tracing::info!(
                    "completed Kernel Run because an extension handled the input"
                );
                return Ok(None);
            }
        };
        Ok(Some(protocol::RunInput::Text(text)))
    }

    /// Closes the Run's queue, then settles the durable Run exactly once for
    /// either a terminal completion or an operational error.
    pub(in crate::runtime) async fn settle(
        &self,
        execution: Result<RunCompletion, KernelError>,
        mut turn_loop: TurnLoop<'_>,
        first_turn_id: TurnId,
    ) -> Result<(RunResult, Option<Arc<SubmissionAnswer>>), KernelError> {
        let kernel = self.kernel;
        // An unconsumed claim returns to the queue before it closes, so the
        // withdrawal below answers its waiter.
        drop(turn_loop.next_queued_item.take());
        self.session.execution.close_queue()?;
        kernel.publish_queue_changed(self.session_id);
        if !matches!(&execution, Ok(completion) if matches!(completion.outcome, AgentOutcome::Succeeded))
        {
            kernel.withdraw_pending(self.session_id)?;
        }
        let settlement = RunSettlement::builder()
            .kernel(kernel)
            .session_id(self.session_id)
            .session(self.session)
            .emitter(self.emitter)
            .run_id(self.run_id)
            .cancellation(self.cancellation)
            .usage(turn_loop.usage);
        match execution {
            Ok(completion) => {
                let answer = settlement
                    .turn_id(completion.turn_id)
                    .outcome(&completion.outcome)
                    .messages(completion.result.messages.clone())
                    .auto_compaction_reason(completion.auto_compaction_reason)
                    .build()
                    .settle()
                    .await?;
                tracing::info!(
                    "settled Kernel Run with outcome {:?} after {} Turns",
                    completion.outcome,
                    completion.result.turns.len()
                );
                Ok((completion.result, Some(answer)))
            }
            Err(error)
                if !turn_loop.user_message_inserted
                    && error.is_cancellation() =>
            {
                if let Some(content) = turn_loop.retained_input {
                    return RetainedInput::builder()
                        .session_id(self.session_id)
                        .session(self.session)
                        .emitter(self.emitter)
                        .run_id(self.run_id)
                        .turn_id(&first_turn_id)
                        .cancellation(self.cancellation)
                        .input_source(self.input_source)
                        .content(content)
                        .build()
                        .settle(kernel)
                        .await;
                }
                settlement
                    .turn_id(first_turn_id)
                    .outcome(&AgentOutcome::Cancelled)
                    .build()
                    .settle()
                    .await?;
                tracing::info!(
                    "cancelled Kernel Run before insertion: {}",
                    error
                );
                Err(KernelError::PromptCancelled)
            }
            Err(error) => {
                let outcome = AgentOutcome::Failed {
                    message: error.to_string(),
                };
                settlement
                    .turn_id(first_turn_id)
                    .outcome(&outcome)
                    .build()
                    .settle()
                    .await?;
                tracing::error!("failed Kernel Run: {}", error);
                Err(error)
            }
        }
    }
}
