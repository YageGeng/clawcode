use super::*;

impl Kernel {
    /// Executes one run while preserving per-session serialization.
    pub async fn run(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
    ) -> Result<RunResult, KernelError> {
        let session = self.session(&request.session_id)?;
        let _run_guard = session.run_gate.lock().await;
        let cancellation = CancellationToken::new();
        *session
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            cancellation.clone();
        let run_id = RunId::try_from(self.id_generator.next(IdKind::Run))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let _active_run =
            ActiveRunLease::acquire(Arc::clone(&session), run_id.clone())?;
        let first_turn_id =
            TurnId::try_from(self.id_generator.next(IdKind::Turn))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink,
            session: Arc::clone(&session),
        };
        self.dispatch(
            ExtensionEvent::Input,
            &request.session_id,
            Some(first_turn_id.clone()),
        )
        .await?;
        self.dispatch(
            ExtensionEvent::BeforeAgentStart,
            &request.session_id,
            Some(first_turn_id.clone()),
        )
        .await?;
        emitter
            .emit(
                first_turn_id.clone(),
                AgentEventPayload::RunStart {
                    run_id: run_id.clone(),
                },
            )
            .await?;
        self.record_operation(
            &session,
            &run_id,
            RecordKind::OperationStarted,
            serde_json::json!({ "operation": "agent" }),
        )?;
        self.dispatch(
            ExtensionEvent::AgentStart,
            &request.session_id,
            Some(first_turn_id.clone()),
        )
        .await?;
        let execution: Result<RunCompletion, KernelError> = async {
            self.model.preflight().await?;

        let pre_prompt_compaction = {
            let history = session
                .history
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            let latest_assistant = history.iter().rev().find_map(|message| {
                if let MessageContent::Assistant { metadata, .. } =
                    &message.content
                {
                    Some((message, metadata))
                } else {
                    None
                }
            });
            latest_assistant
                .and_then(|(message, metadata)| {
                    self.compaction_policy
                        .overflow_recovery(metadata, self.model.profile())
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
                            self.model.profile().context_tokens,
                            self.compaction_policy,
                        )
                        .then_some((CompactionReason::Threshold, None))
                })
        };
        if let Some((reason, excluded_message_id)) = pre_prompt_compaction {
            // Pi checks the previous Assistant before persisting a newly
            // submitted user message, preventing an avoidable overflow call.
            self.perform_compaction(
                CompactionExecution::builder()
                    .session_id(&request.session_id)
                    .session(&session)
                    .sink(Arc::clone(&emitter.sink))
                    .reason(reason)
                    .cancellation(&cancellation)
                    .excluded_message_id(excluded_message_id.as_ref())
                    .build(),
            )
            .await?;
        }

        let mut produced_messages = Vec::new();
        let mut turns = Vec::new();
        let mut next_initial_input = Some(request.input);
        let mut next_queued_item = None;
        let mut next_turn_id = Some(first_turn_id.clone());
        let mut retry_state = RetryState::default();
        let mut overflow_recovery_attempted = false;

        while turns.len() < self.max_turns {
            let queued_item = next_queued_item.take();
            let turn_id = queued_item
                .as_ref()
                .map(|item: &PendingQueueItem| {
                    item.queued.message.identity.turn_id.clone()
                })
                .or_else(|| next_turn_id.take())
                .map_or_else(
                    || {
                        TurnId::try_from(self.id_generator.next(IdKind::Turn))
                            .map_err(|error| {
                                KernelError::Protocol(error.to_string())
                            })
                    },
                    Ok,
                )?;
            let turn_started_at = self.clock.now();
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::TurnStart {
                        run_id: run_id.clone(),
                    },
                )
                .await?;
            self.dispatch(
                ExtensionEvent::TurnStart,
                &request.session_id,
                Some(turn_id.clone()),
            )
            .await?;

            let queued_entry_id =
                queued_item.as_ref().map(|item| item.entry_id.clone());
            let user_message = match queued_item.as_ref() {
                Some(item) => Some(item.queued.message.clone()),
                None => match next_initial_input.take() {
                    Some(input) => {
                        let timestamp = self.clock.now();
                        Some(AgentMessage {
                            identity: MessageIdentity {
                                message_id: self.message_id()?,
                                turn_id: turn_id.clone(),
                            },
                            timing: MessageTiming::try_from((
                                timestamp, timestamp, timestamp,
                            ))
                            .map_err(|error| {
                                KernelError::Protocol(error.to_string())
                            })?,
                            content: MessageContent::User {
                                blocks: vec![ContentBlock::Text {
                                    text: input,
                                }],
                            },
                        })
                    }
                    None => None,
                },
            };
            if let Some(message) = user_message {
                let timestamp = message.timing.timestamp_ms;
                emitter
                    .emit_at(
                        turn_id.clone(),
                        timestamp,
                        AgentEventPayload::MessageStart {
                            message_id: message.identity.message_id.clone(),
                        },
                    )
                    .await?;
                self.dispatch(
                    ExtensionEvent::MessageStart,
                    &request.session_id,
                    Some(turn_id.clone()),
                )
                .await?;
                match queued_entry_id {
                    Some(entry_id) => {
                        self.persist_message_at(&session, &message, entry_id)?
                    }
                    None => self.persist_message(&session, &message)?,
                }
                if let Some(title) =
                    self.ensure_default_title(&session, &message)?
                {
                    emitter
                        .emit_at(
                            turn_id.clone(),
                            timestamp,
                            AgentEventPayload::SessionTitleChanged { title },
                        )
                        .await?;
                }
                if let Some(item) = queued_item {
                    self.record_queue_cancellation(
                        &session,
                        Some(&run_id),
                        item.queued.queue_id.clone(),
                        QueueCancellationReason::Consumed,
                    )?;
                    session
                        .queue
                        .lock()
                        .map_err(|_poison_error| KernelError::Poisoned)?
                        .remove(&item.queued.queue_id);
                }
                emitter
                    .emit_at(
                        turn_id.clone(),
                        timestamp,
                        AgentEventPayload::MessageEnd {
                            message: message.clone(),
                        },
                    )
                    .await?;
                self.dispatch(
                    ExtensionEvent::MessageEnd,
                    &request.session_id,
                    Some(turn_id.clone()),
                )
                .await?;
                produced_messages.push(message);
            }

            let context_directive = self
                .dispatch(
                    ExtensionEvent::Context,
                    &request.session_id,
                    Some(turn_id.clone()),
                )
                .await?;
            let mut history = session
                .history
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .clone();
            // Failed Assistant attempts remain replayable in Store but are
            // excluded from the next active provider context, matching pi.
            history.retain(|message| {
                !matches!(
                    &message.content,
                    MessageContent::Assistant { metadata, .. }
                        if metadata.error.is_some()
                )
            });
            let system_prompt = self.system_prompt_factory.create(
                SystemPromptContext::builder()
                    .cwd(session.cwd.clone())
                    .turn_id(turn_id.clone())
                    .timestamp_ms(turn_started_at)
                    .tools(session.tools.definitions())
                    .skills(
                        self.skills
                            .as_ref()
                            .map(skill::SkillCatalog::descriptors)
                            .unwrap_or_default(),
                    )
                    .project(session.project_context.clone())
                    .build(),
            )?;
            history.insert(0, system_prompt);
            history.extend(context_directive.injections);
            self.dispatch(
                ExtensionEvent::BeforeProviderHeaders,
                &request.session_id,
                Some(turn_id.clone()),
            )
            .await?;
            self.dispatch(
                ExtensionEvent::BeforeProviderRequest,
                &request.session_id,
                Some(turn_id.clone()),
            )
            .await?;
            let request_for_model = ModelRequest {
                messages: history,
                tools: session.tools.definitions(),
            };
            let assistant_id = self.message_id()?;
            let assistant_timestamp = self.clock.now();
            let mut attempt = AssistantAttempt::new(
                assistant_id.clone(),
                turn_id.clone(),
                self.model.profile().clone(),
                assistant_timestamp,
            );
            emitter
                .emit_at(
                    turn_id.clone(),
                    assistant_timestamp,
                    AgentEventPayload::MessageStart {
                        message_id: assistant_id.clone(),
                    },
                )
                .await?;
            self.dispatch(
                ExtensionEvent::MessageStart,
                &request.session_id,
                Some(turn_id.clone()),
            )
            .await?;

            let settlement = match self
                .model
                .stream(request_for_model, cancellation.clone())
                .await
            {
                Ok(mut stream) => {
                    self.dispatch(
                        ExtensionEvent::AfterProviderResponse,
                        &request.session_id,
                        Some(turn_id.clone()),
                    )
                    .await?;
                    loop {
                        let item = tokio::select! {
                            () = cancellation.cancelled() => {
                                break AssistantAttemptSettlement::Cancelled {
                                    settled_at_ms: self.clock.now(),
                                };
                            }
                            item = stream.next() => item,
                        };
                        let Some(item) = item else {
                            break AssistantAttemptSettlement::Completed {
                                settled_at_ms: self.clock.now(),
                            };
                        };
                        let item = match item {
                            Ok(item) => item,
                            Err(error) => {
                                break AssistantAttemptSettlement::Failed {
                                    error,
                                    settled_at_ms: self.clock.now(),
                                };
                            }
                        };
                        let timestamp = self.clock.now();
                        if let Err(error) = attempt.apply(&item, timestamp) {
                            break AssistantAttemptSettlement::Failed {
                                error,
                                settled_at_ms: self.clock.now(),
                            };
                        }
                        match item {
                            ModelStreamEvent::TextDelta(delta) => {
                                emitter
                                    .emit_at(
                                        turn_id.clone(),
                                        timestamp,
                                        AgentEventPayload::MessageTextDelta {
                                            message_id: assistant_id.clone(),
                                            delta,
                                        },
                                    )
                                    .await?;
                                self.dispatch(
                                    ExtensionEvent::MessageUpdate,
                                    &request.session_id,
                                    Some(turn_id.clone()),
                                )
                                .await?;
                            }
                            ModelStreamEvent::ReasoningDelta(delta) => {
                                emitter
                                    .emit_at(
                                        turn_id.clone(),
                                        timestamp,
                                        AgentEventPayload::MessageReasoningDelta {
                                            message_id: assistant_id.clone(),
                                            delta,
                                        },
                                    )
                                    .await?;
                                self.dispatch(
                                    ExtensionEvent::MessageUpdate,
                                    &request.session_id,
                                    Some(turn_id.clone()),
                                )
                                .await?;
                            }
                            ModelStreamEvent::ToolCall(_call) => {
                                self.dispatch(
                                    ExtensionEvent::ToolCall,
                                    &request.session_id,
                                    Some(turn_id.clone()),
                                )
                                .await?;
                            }
                            ModelStreamEvent::Finished(_final) => {}
                        }
                    }
                }
                Err(ModelError::Cancelled) => {
                    AssistantAttemptSettlement::Cancelled {
                        settled_at_ms: self.clock.now(),
                    }
                }
                Err(error) => AssistantAttemptSettlement::Failed {
                    error,
                    settled_at_ms: self.clock.now(),
                },
            };
            let attempt_result = attempt.settle(settlement)?;
            let (assistant, tool_calls, stop_reason, failure, mut cancelled) =
                match attempt_result {
                    AssistantAttemptResult::Completed {
                        message,
                        tool_calls,
                        stop_reason,
                    } => (message, tool_calls, stop_reason, None, false),
                    AssistantAttemptResult::Failed { message, failure } => (
                        message,
                        Vec::new(),
                        StopReason::Error,
                        Some(failure),
                        false,
                    ),
                    AssistantAttemptResult::Cancelled { message } => {
                        (message, Vec::new(), StopReason::Cancelled, None, true)
                    }
                };
            let (usage, overflow_recovery) = match &assistant.content {
                MessageContent::Assistant { metadata, .. } => (
                    metadata.usage.clone(),
                    self.compaction_policy
                        .overflow_recovery(metadata, self.model.profile()),
                ),
                MessageContent::System { .. }
                | MessageContent::User { .. }
                | MessageContent::ToolResult { .. }
                | MessageContent::Extension { .. } => unreachable!(
                    "AssistantAttempt always produces an Assistant message"
                ),
            };
            self.persist_message(&session, &assistant)?;
            produced_messages.push(assistant.clone());
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::MessageEnd { message: assistant },
                )
                .await?;
            self.dispatch(
                ExtensionEvent::MessageEnd,
                &request.session_id,
                Some(turn_id.clone()),
            )
            .await?;
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::UsageUpdated {
                        usage,
                        context_window: self.model.profile().context_tokens,
                    },
                )
                .await?;

            let tool_batch = if cancelled {
                ToolBatchResult::default()
            } else {
                self.execute_tool_batch(
                    ToolBatch::builder()
                        .session_id(&request.session_id)
                        .run_id(&run_id)
                        .turn_id(&turn_id)
                        .session(&session)
                        .emitter(&emitter)
                        .cancellation(&cancellation)
                        .calls(tool_calls.clone())
                        .build(),
                )
                .await?
            };
            cancelled |= tool_batch.cancelled;
            for message in tool_batch.messages {
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::MessageStart {
                            message_id: message.identity.message_id.clone(),
                        },
                    )
                    .await?;
                self.dispatch(
                    ExtensionEvent::MessageStart,
                    &request.session_id,
                    Some(turn_id.clone()),
                )
                .await?;
                self.dispatch(
                    ExtensionEvent::ToolResult,
                    &request.session_id,
                    Some(turn_id.clone()),
                )
                .await?;
                self.persist_message(&session, &message)?;
                produced_messages.push(message.clone());
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::MessageEnd { message },
                    )
                    .await?;
                self.dispatch(
                    ExtensionEvent::MessageEnd,
                    &request.session_id,
                    Some(turn_id.clone()),
                )
                .await?;
            }

            let turn_ended_at = self.clock.now();
            let turn = TurnRecord {
                identity: TurnIdentity {
                    run_id: run_id.clone(),
                    turn_id: turn_id.clone(),
                },
                timing: TurnTiming::try_from((turn_started_at, turn_ended_at))
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                outcome: if cancelled {
                    TurnOutcome::Cancelled
                } else if let Some(failure) = &failure {
                    TurnOutcome::Failed {
                        message: failure.summary.clone(),
                    }
                } else {
                    TurnOutcome::Completed { stop_reason }
                },
            };
            self.record_operation(
                &session,
                &run_id,
                RecordKind::StepAttempt,
                serde_json::to_value(&turn)?,
            )?;
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::TurnEnd { turn: turn.clone() },
                )
                .await?;
            self.dispatch(
                ExtensionEvent::TurnEnd,
                &request.session_id,
                Some(turn_id.clone()),
            )
            .await?;
            turns.push(turn);

            if cancelled {
                return Ok(RunCompletion::builder()
                    .result(RunResult {
                        run_id: run_id.clone(),
                        messages: produced_messages,
                        turns,
                    })
                    .turn_id(turn_id)
                    .outcome(AgentOutcome::Cancelled)
                    .build());
            }

            if overflow_recovery == Some(OverflowRecovery::CompactAndRetry)
                && !overflow_recovery_attempted
            {
                overflow_recovery_attempted = true;
                self.perform_compaction(
                    CompactionExecution::builder()
                        .session_id(&request.session_id)
                        .session(&session)
                        .sink(Arc::clone(&emitter.sink))
                        .reason(CompactionReason::Overflow)
                        .cancellation(&cancellation)
                        .excluded_message_id(Some(&assistant_id))
                        .build(),
                )
                .await?;
                next_turn_id = Some(
                    TurnId::try_from(self.id_generator.next(IdKind::Turn))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?,
                );
                continue;
            }

            if let Some(retry_failure) = failure.as_ref()
                && RetryClassifier::classify(retry_failure)
                    == ModelRetryDisposition::Retryable
                && let Some(schedule) = retry_state.schedule(&self.retry_policy)
            {
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::RetryScheduled {
                            run_id: run_id.clone(),
                            attempt: schedule.attempt,
                            max_attempts: schedule.max_attempts,
                            delay_ms: schedule.delay_ms,
                            error: retry_failure.summary.clone(),
                        },
                    )
                    .await?;
                let backoff = tokio::time::sleep(Duration::from_millis(
                    schedule.delay_ms,
                ));
                tokio::pin!(backoff);
                let retry_cancelled = tokio::select! {
                    () = cancellation.cancelled() => true,
                    () = &mut backoff => false,
                };
                if retry_cancelled {
                    emitter
                        .emit(
                            turn_id.clone(),
                            AgentEventPayload::RetryEnd {
                                run_id: run_id.clone(),
                                attempt: schedule.attempt,
                                success: false,
                                final_error: Some(
                                    "Retry cancelled".to_string(),
                                ),
                            },
                        )
                        .await?;
                    return Ok(RunCompletion::builder()
                        .result(RunResult {
                            run_id: run_id.clone(),
                            messages: produced_messages,
                            turns,
                        })
                        .turn_id(turn_id)
                        .outcome(AgentOutcome::Cancelled)
                        .build());
                }

                let retry_turn_id =
                    TurnId::try_from(self.id_generator.next(IdKind::Turn))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?;
                emitter
                    .emit(
                        retry_turn_id.clone(),
                        AgentEventPayload::RetryStart {
                            run_id: run_id.clone(),
                            attempt: schedule.attempt,
                            max_attempts: schedule.max_attempts,
                        },
                    )
                    .await?;
                next_turn_id = Some(retry_turn_id);
                continue;
            }

            if failure.is_none() && retry_state.attempts() > 0 {
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::RetryEnd {
                            run_id: run_id.clone(),
                            attempt: retry_state.attempts(),
                            success: true,
                            final_error: None,
                        },
                    )
                    .await?;
                retry_state = RetryState::default();
            } else if let Some(retry_failure) = failure.as_ref()
                && retry_state.attempts() > 0
            {
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::RetryEnd {
                            run_id: run_id.clone(),
                            attempt: retry_state.attempts(),
                            success: false,
                            final_error: Some(retry_failure.summary.clone()),
                        },
                    )
                    .await?;
            }

            if let Some(failure) = failure {
                return Ok(RunCompletion::builder()
                    .result(RunResult {
                        run_id: run_id.clone(),
                        messages: produced_messages,
                        turns,
                    })
                    .turn_id(turn_id)
                    .outcome(AgentOutcome::Failed {
                        message: failure.summary,
                    })
                    .build());
            }

            let should_continue_for_tools = !tool_calls.is_empty();
            next_queued_item = {
                let queue = session
                    .queue
                    .lock()
                    .map_err(|_poison_error| KernelError::Poisoned)?;
                queue.next(!should_continue_for_tools)
            };
            let should_continue =
                should_continue_for_tools || next_queued_item.is_some();
            if !should_continue {
                let should_threshold_compact =
                    ContextUsageEstimate::from_history(
                        &session
                            .history
                            .lock()
                            .map_err(|_poison_error| KernelError::Poisoned)?,
                    )
                    .should_compact(
                        self.model.profile().context_tokens,
                        self.compaction_policy,
                    );
                let auto_compaction_reason = match overflow_recovery {
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
                return Ok(RunCompletion::builder()
                    .result(RunResult {
                        run_id: run_id.clone(),
                        messages: produced_messages,
                        turns,
                    })
                    .turn_id(turn_id)
                    .outcome(AgentOutcome::Succeeded)
                    .auto_compaction_reason(auto_compaction_reason)
                    .build());
            }
        }

            Err(KernelError::TurnLimit(self.max_turns))
        }
        .await;

        match execution {
            Ok(completion) => {
                RunSettlement::builder()
                    .kernel(self)
                    .session_id(&request.session_id)
                    .session(&session)
                    .emitter(&emitter)
                    .run_id(&run_id)
                    .turn_id(completion.turn_id)
                    .cancellation(&cancellation)
                    .outcome(&completion.outcome)
                    .auto_compaction_reason(completion.auto_compaction_reason)
                    .build()
                    .settle()
                    .await?;
                Ok(completion.result)
            }
            Err(error) => {
                let outcome = AgentOutcome::Failed {
                    message: error.to_string(),
                };
                RunSettlement::builder()
                    .kernel(self)
                    .session_id(&request.session_id)
                    .session(&session)
                    .emitter(&emitter)
                    .run_id(&run_id)
                    .turn_id(first_turn_id)
                    .cancellation(&cancellation)
                    .outcome(&outcome)
                    .build()
                    .settle()
                    .await?;
                Err(error)
            }
        }
    }
}
