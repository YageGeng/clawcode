use super::*;

/// Internal correlated result shared by prefixed prompts and direct ACP requests.
pub(super) struct UserBashExecution {
    pub(super) run_id: RunId,
    pub(super) message: AgentMessage,
    result: UserBashResult,
}

impl Kernel {
    /// Routes a prefixed run request into user-bash before ordinary input hooks.
    pub(super) async fn try_execute_user_bash_request(
        &self,
        request: &RunRequest,
        sink: Arc<dyn EventSink>,
        insertion: Option<&InsertionNotice>,
    ) -> Result<Option<RunResult>, KernelError> {
        let Some(input) = request
            .input
            .slash_command_text()
            .and_then(UserBashInput::parse_prefixed)
        else {
            return Ok(None);
        };
        let execution = self
            .execute_user_bash_operation(
                self.session(&request.session_id)?,
                input,
                sink,
                insertion,
            )
            .await?;
        Ok(Some(RunResult {
            run_id: execution.run_id,
            messages: vec![execution.message],
            turns: Vec::new(),
        }))
    }

    /// Executes and persists one server-side user-bash request without client callbacks.
    pub async fn execute_user_bash(
        &self,
        request: UserBashRequest,
        sink: Arc<dyn EventSink>,
    ) -> Result<UserBashResult, KernelError> {
        let session = self.session(&request.session_id)?;
        let execution = self
            .execute_user_bash_operation(
                session,
                UserBashInput {
                    command: request.command,
                    exclude_from_context: request.exclude_from_context,
                },
                sink,
                None,
            )
            .await?;
        Ok(execution.result)
    }

    /// Serializes one bash operation, runs its extension hook, and materializes its message.
    pub(super) async fn execute_user_bash_operation(
        &self,
        session: Arc<Session>,
        input: UserBashInput,
        sink: Arc<dyn EventSink>,
        insertion: Option<&InsertionNotice>,
    ) -> Result<UserBashExecution, KernelError> {
        // A cancel or close while this shell waits for the gate must reject it
        // before any process is spawned.
        let (_run_guard, cancellation) = session
            .acquire_operation_for_prompt(
                None,
                WaiterCancellation::from(insertion),
            )
            .await?;
        let run_id = RunId::try_from(self.id_generator.next(IdKind::Run))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let turn_id = TurnId::try_from(self.id_generator.next(IdKind::Turn))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let _active_run = session.execution.install_run(ActiveRunContext {
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
            sink: Arc::clone(&sink),
        })?;
        // A client prompt cancelled during admission must not start a shell.
        if insertion.is_some() && cancellation.is_cancelled() {
            return Err(KernelError::PromptCancelled);
        }
        let message_id = self.message_id()?;
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink,
            session: Arc::clone(&session),
        };
        let started_at_ms = self.clock.now();
        emitter
            .emit_at(
                turn_id.clone(),
                started_at_ms,
                AgentEventPayload::UserBashStarted {
                    run_id: run_id.clone(),
                    message_id: message_id.clone(),
                    command: input.command.clone(),
                    cwd: session.cwd.clone(),
                    exclude_from_context: input.exclude_from_context,
                },
            )
            .await?;
        if let Some(notice) = insertion {
            notice.inserted(&message_id);
        }
        tracing::info!(
            "started user bash Run {} with message {}",
            run_id,
            message_id
        );
        let execution = UserBashOperation::builder()
            .session(&session)
            .emitter(&emitter)
            .run_id(&run_id)
            .turn_id(&turn_id)
            .message_id(&message_id)
            .cancellation(cancellation)
            .started_at_ms(started_at_ms)
            .input(input)
            .build()
            .execute(self)
            .await;
        // Every accepted shell reaches exactly one foreground terminal state.
        let (outcome, result_emitted) = match &execution {
            Ok(execution) if execution.result.cancelled => {
                (AgentOutcome::Cancelled, true)
            }
            Ok(_execution) => (AgentOutcome::Succeeded, true),
            Err(error) if error.is_cancellation() => {
                (AgentOutcome::Cancelled, false)
            }
            Err(error) => {
                tracing::warn!(
                    "user bash Run {} failed after acceptance: {}",
                    run_id,
                    error
                );
                (
                    AgentOutcome::Failed {
                        message: error.to_string(),
                    },
                    false,
                )
            }
        };
        tracing::info!(
            "finished user bash Run {} with outcome {:?}",
            run_id,
            outcome
        );
        emitter
            .emit(
                turn_id.clone(),
                AgentEventPayload::UserBashFinished {
                    run_id: run_id.clone(),
                    message_id,
                    outcome,
                    result_emitted,
                },
            )
            .await?;
        execution
    }
}

/// One accepted user shell from its extension hook through its durable result.
#[derive(typed_builder::TypedBuilder)]
struct UserBashOperation<'a> {
    session: &'a Arc<Session>,
    emitter: &'a EventEmitter,
    run_id: &'a RunId,
    turn_id: &'a TurnId,
    message_id: &'a MessageId,
    cancellation: CancellationToken,
    started_at_ms: TimestampMs,
    input: UserBashInput,
}

impl UserBashOperation<'_> {
    /// Runs the hook or executor, then persists and emits the result under the accepted id.
    async fn execute(
        self,
        kernel: &Kernel,
    ) -> Result<UserBashExecution, KernelError> {
        let extension_context = kernel.extension_context(
            self.session,
            Some(self.run_id),
            Some(self.turn_id),
        )?;
        let event = protocol::UserBashEvent {
            command: self.input.command.clone(),
            exclude_from_context: self.input.exclude_from_context,
            cwd: self.session.cwd.clone(),
        };
        let mut output_at_ms = None;
        let result = match self
            .session
            .extensions
            .runtime_ref()
            .emit_user_bash(&event, &extension_context)
            .await
        {
            Some(result) => {
                output_at_ms = Some(kernel.clock.now());
                result
            }
            None => {
                // The executor publishes synchronously; the latest snapshot is
                // forwarded as an event, and older unsent ones are skipped.
                let (snapshots, mut latest) =
                    tokio::sync::watch::channel(None::<String>);
                let output_at = &mut output_at_ms;
                let executed = BashExecutor::execute(
                    BashExecutionRequest::builder()
                        .command(self.input.command.clone())
                        .cwd(self.session.cwd.clone())
                        .session_id(
                            extension_context.invocation.session_id.clone(),
                        )
                        .turn_id(self.turn_id.clone())
                        .cancellation(self.cancellation)
                        .build(),
                    move |snapshot| {
                        if output_at.is_none() && !snapshot.output.is_empty() {
                            *output_at = Some(kernel.clock.now());
                        }
                        snapshots.send_replace(Some(snapshot.output));
                    },
                );
                // Ends when the executor drops its sender, after the final snapshot.
                let forward = async {
                    while latest.changed().await.is_ok() {
                        let Some(output) = latest.borrow_and_update().clone()
                        else {
                            continue;
                        };
                        if let Err(error) = self
                            .emitter
                            .emit(
                                self.turn_id.clone(),
                                AgentEventPayload::UserBashOutput {
                                    run_id: self.run_id.clone(),
                                    message_id: self.message_id.clone(),
                                    output,
                                },
                            )
                            .await
                        {
                            tracing::warn!(
                                "failed to report user bash output for Run {}: {}",
                                self.run_id,
                                error
                            );
                        }
                    }
                };
                let (executed, ()) = tokio::join!(executed, forward);
                executed?.result
            }
        };
        let ended_at_ms = kernel.clock.now();
        let message = AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id.clone(),
                turn_id: self.turn_id.clone(),
            },
            timing: MessageTiming::try_from((
                self.started_at_ms,
                output_at_ms.unwrap_or(ended_at_ms),
                ended_at_ms,
            ))
            .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::BashExecution {
                bash: BashExecutionMessage::builder()
                    .command(self.input.command)
                    .result(result.clone())
                    .exclude_from_context(self.input.exclude_from_context)
                    .cwd(self.session.cwd.clone())
                    .build(),
            },
        };
        kernel.persist_message(self.session, &message)?;
        // MessageEnd acknowledges the completed Bash transcript entry.
        self.session.sync_store()?;
        self.emitter
            .emit_at(
                self.turn_id.clone(),
                ended_at_ms,
                AgentEventPayload::MessageEnd {
                    message: message.clone(),
                },
            )
            .await?;
        Ok(UserBashExecution {
            run_id: self.run_id.clone(),
            message,
            result,
        })
    }
}
