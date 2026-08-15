use super::*;

impl Kernel {
    /// Executes one run with an explicit input source for trusted host callers.
    pub(in crate::runtime) async fn run_with_source(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
        input_source: protocol::InputSource,
    ) -> Result<RunResult, KernelError> {
        // Pi handles `!` and `!!` before ordinary input hooks and agent Turns.
        if let Some(result) = self
            .try_execute_user_bash_request(&request, Arc::clone(&sink))
            .await?
        {
            return Ok(result);
        }
        let session = self.session(&request.session_id)?;
        let _run_guard = session.acquire_operation().await?;
        let cancellation = CancellationToken::new();
        *session
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            cancellation.clone();
        let run_id = RunId::try_from(self.id_generator.next(IdKind::Run))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let first_turn_id =
            TurnId::try_from(self.id_generator.next(IdKind::Turn))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let _active_run = ActiveRunLease::acquire(
            Arc::clone(&session),
            run_id.clone(),
            first_turn_id.clone(),
            Arc::clone(&sink),
        )?;
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink,
            session: Arc::clone(&session),
        };
        let extension_context = self.extension_context(
            &session,
            Some(&run_id),
            Some(&first_turn_id),
        )?;
        let input = session
            .extensions
            .emit_input(
                protocol::InputEvent {
                    text: request.input.clone(),
                    source: input_source,
                    streaming_behavior: None,
                },
                &extension_context,
            )
            .await;
        let run_input = match input {
            protocol::InputResult::Continue => request.input.clone(),
            protocol::InputResult::Transform { text } => text,
            protocol::InputResult::Handled => {
                return Ok(RunResult {
                    run_id,
                    messages: Vec::new(),
                    turns: Vec::new(),
                });
            }
        };
        // Pi expands prompt templates only after extensions have transformed input.
        let run_input = session
            .prompt_templates
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .expand(&run_input)
            .unwrap_or(run_input);
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
        let execution: Result<RunCompletion, KernelError> = async {
            let initial_tools = session.tool_state.snapshot()?.active_registry()?;
            let initial_skills = session
                .skills
                .read()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .as_ref()
                .map(skill::SkillCatalog::descriptors)
                .unwrap_or_default();
            let initial_system_prompt = self.system_prompt_factory.create(
                SystemPromptContext::builder()
                    .cwd(session.cwd.clone())
                    .turn_id(first_turn_id.clone())
                    .timestamp_ms(self.clock.now())
                    .tools(initial_tools.definitions())
                    .skills(initial_skills.clone())
                    .project(session.project_context.clone())
                    .build(),
            )?;
            let before_agent = session
                .extensions
                .emit_before_agent_start(
                    protocol::BeforeAgentStartEvent {
                        prompt: run_input.clone(),
                        system_prompt: initial_system_prompt
                            .content
                            .text_content()
                            .unwrap_or_default(),
                        skills: initial_skills
                            .into_iter()
                            .map(|skill| skill.name)
                            .collect(),
                    },
                    &extension_context,
                )
                .await;
            session
                .extensions
                .emit_agent_start(
                    &protocol::AgentStartEvent,
                    &extension_context,
                )
                .await;
            let initial_model = session
                .model
                .read()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .clone();

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
                        .overflow_recovery(metadata, initial_model.profile())
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
                            initial_model.profile().context_tokens,
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
        let mut next_initial_input = Some(run_input.clone());
            let mut before_agent = Some(before_agent);
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
            // Model selection is snapshotted once so host changes made during
            // this Turn apply only to the next Turn.
            let turn_model = session
                .model
                .read()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .clone();
            turn_model.preflight().await?;
            *session
                .diagnostic_turn_id
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)? =
                Some(turn_id.clone());
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::TurnStart {
                        run_id: run_id.clone(),
                    },
                )
                .await?;
            let extension_context = self.extension_context(
                &session,
                Some(&run_id),
                Some(&turn_id),
            )?;
            session
                .extensions
                .emit_turn_start(
                    &protocol::TurnStartEvent {
                        turn_index: turns.len(),
                        timestamp_ms: turn_started_at,
                    },
                    &extension_context,
                )
                .await;

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
                session
                    .extensions
                    .emit_message_start(
                        &protocol::MessageStartEvent {
                            message: message.clone(),
                        },
                        &extension_context,
                    )
                    .await;
                let message = session
                    .extensions
                    .emit_message_end(
                        protocol::MessageEndEvent {
                            message: message.clone(),
                        },
                        &extension_context,
                    )
                    .await;
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
                produced_messages.push(message);
            }

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
            // One immutable registry snapshot defines the complete Turn.
            let turn_tools =
                Arc::new(session.tool_state.snapshot()?.active_registry()?);
            let skill_descriptors = session
                .skills
                .read()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .as_ref()
                .map(skill::SkillCatalog::descriptors)
                .unwrap_or_default();
            let mut system_prompt = self.system_prompt_factory.create(
                SystemPromptContext::builder()
                    .cwd(session.cwd.clone())
                    .turn_id(turn_id.clone())
                    .timestamp_ms(turn_started_at)
                    .tools(turn_tools.definitions())
                    .skills(skill_descriptors.clone())
                    .project(session.project_context.clone())
                    .build(),
            )?;
            if let Some(before_agent) = before_agent.take() {
                if let Some(replacement) = before_agent.system_prompt {
                    system_prompt.content = MessageContent::System {
                        blocks: vec![ContentBlock::Text { text: replacement }],
                    };
                }
                for draft in before_agent.messages {
                    let message = self
                        .materialize_extension_message(&turn_id, draft)?;
                    session
                        .extensions
                        .emit_message_start(
                            &protocol::MessageStartEvent {
                                message: message.clone(),
                            },
                            &extension_context,
                        )
                        .await;
                    let identity = message.identity.clone();
                    let message = session
                        .extensions
                        .emit_message_end(
                            protocol::MessageEndEvent { message },
                            &extension_context,
                        )
                        .await;
                    if message.identity != identity
                        || !matches!(
                            message.content,
                            MessageContent::Extension { .. }
                        )
                    {
                        return Err(KernelError::Protocol(
                            "message_end must preserve Extension identity and role"
                                .to_string(),
                        ));
                    }
                    self.persist_message(&session, &message)?;
                    emitter
                        .emit(
                            turn_id.clone(),
                            AgentEventPayload::MessageEnd {
                                message: message.clone(),
                            },
                        )
                        .await?;
                    history.push(message.clone());
                    produced_messages.push(message);
                }
            }
            history.insert(0, system_prompt);
            let mut request_for_model = ModelRequest {
                messages: history,
                tools: turn_tools.definitions(),
            };
            if let Some(messages) = session
                .extensions
                .emit_context(
                    protocol::ContextEvent {
                        request: request_for_model.clone(),
                    },
                    &extension_context,
                )
                .await
                .messages
            {
                request_for_model.messages = messages;
            }
            let assistant_id = self.message_id()?;
            let assistant_timestamp = self.clock.now();
            let mut attempt = AssistantAttempt::new(
                assistant_id.clone(),
                turn_id.clone(),
                turn_model.profile().clone(),
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
            session
                .extensions
                .emit_message_start(
                    &protocol::MessageStartEvent {
                        message: attempt.snapshot(assistant_timestamp)?,
                    },
                    &extension_context,
                )
                .await;

            let settlement = match turn_model
                .stream_with_hooks(
                    request_for_model,
                    cancellation.clone(),
                    Some(self.completion_hooks(&session, &run_id, &turn_id)?),
                )
                .await
            {
                Ok(mut stream) => {
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
                                            delta: delta.clone(),
                                        },
                                    )
                                    .await?;
                                session.extensions.emit_message_update(
                                    &protocol::MessageUpdateEvent {
                                        message: attempt.snapshot(timestamp)?,
                                        update: protocol::MessageUpdate::Text {
                                            delta,
                                        },
                                    },
                                    &extension_context,
                                ).await;
                            }
                            ModelStreamEvent::ReasoningDelta(delta) => {
                                emitter
                                    .emit_at(
                                        turn_id.clone(),
                                        timestamp,
                                        AgentEventPayload::MessageReasoningDelta {
                                            message_id: assistant_id.clone(),
                                            delta: delta.clone(),
                                        },
                                    )
                                    .await?;
                                session.extensions.emit_message_update(
                                    &protocol::MessageUpdateEvent {
                                        message: attempt.snapshot(timestamp)?,
                                        update: protocol::MessageUpdate::Reasoning {
                                            delta,
                                        },
                                    },
                                    &extension_context,
                                ).await;
                            }
                            ModelStreamEvent::ToolCall(call) => {
                                session.extensions.emit_message_update(
                                    &protocol::MessageUpdateEvent {
                                        message: attempt.snapshot(timestamp)?,
                                        update: protocol::MessageUpdate::ToolCall {
                                            call,
                                        },
                                    },
                                    &extension_context,
                                ).await;
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
            let original_identity = assistant.identity.clone();
            let assistant = session.extensions.emit_message_end(
                protocol::MessageEndEvent { message: assistant },
                &extension_context,
            ).await;
            if assistant.identity != original_identity
                || !matches!(assistant.content, MessageContent::Assistant { .. })
            {
                return Err(KernelError::Protocol(
                    "message_end must preserve Assistant identity and role".to_string(),
                ));
            }
            let (usage, overflow_recovery) = match &assistant.content {
                MessageContent::Assistant { metadata, .. } => (
                    metadata.usage.clone(),
                    self.compaction_policy
                        .overflow_recovery(metadata, turn_model.profile()),
                ),
                MessageContent::System { .. }
                | MessageContent::User { .. }
                | MessageContent::ToolResult { .. }
                | MessageContent::BashExecution { .. }
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
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::UsageUpdated {
                        usage,
                        context_window: turn_model.profile().context_tokens,
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
                        .tools(Arc::clone(&turn_tools))
                        .calls(tool_calls.clone())
                        .build(),
                )
                .await?
            };
            cancelled |= tool_batch.cancelled;
            let tool_terminated = tool_batch.terminate;
            let turn_tool_results = tool_batch.results.clone();
            for message in tool_batch.messages {
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::MessageStart {
                            message_id: message.identity.message_id.clone(),
                        },
                    )
                    .await?;
                session.extensions.emit_message_start(
                    &protocol::MessageStartEvent { message: message.clone() },
                    &extension_context,
                ).await;
                let original_identity = message.identity.clone();
                let message = session.extensions.emit_message_end(
                    protocol::MessageEndEvent { message },
                    &extension_context,
                ).await;
                if message.identity != original_identity
                    || !matches!(message.content, MessageContent::ToolResult { .. })
                {
                    return Err(KernelError::Protocol(
                        "message_end must preserve ToolResult identity and role".to_string(),
                    ));
                }
                self.persist_message(&session, &message)?;
                produced_messages.push(message.clone());
                emitter
                    .emit(
                        turn_id.clone(),
                        AgentEventPayload::MessageEnd { message },
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
            session.extensions.emit_turn_end(
                &protocol::TurnEndEvent {
                    turn: turn.clone(),
                    tool_results: turn_tool_results,
                },
                &extension_context,
            ).await;
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

            let should_continue_for_tools =
                !tool_terminated && !tool_calls.is_empty();
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
                        turn_model.profile().context_tokens,
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
                    .messages(completion.result.messages.clone())
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
