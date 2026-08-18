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
            )
            .await?;
        Ok(execution.result)
    }

    /// Serializes one bash operation, runs its extension hook, and materializes its message.
    pub(super) async fn execute_user_bash_operation(
        &self,
        session: Arc<SessionRuntime>,
        input: UserBashInput,
        sink: Arc<dyn EventSink>,
    ) -> Result<UserBashExecution, KernelError> {
        let _run_guard = session.acquire_operation().await?;
        let cancellation = CancellationToken::new();
        *session
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? =
            cancellation.clone();
        let run_id = RunId::try_from(self.id_generator.next(IdKind::Run))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let turn_id = TurnId::try_from(self.id_generator.next(IdKind::Turn))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let _active_run = ActiveRunLease::acquire(
            Arc::clone(&session),
            run_id.clone(),
            turn_id.clone(),
            Arc::clone(&sink),
        )?;
        let extension_context =
            self.extension_context(&session, Some(&run_id), Some(&turn_id))?;
        let event = protocol::UserBashEvent {
            command: input.command.clone(),
            exclude_from_context: input.exclude_from_context,
            cwd: session.cwd.clone(),
        };
        let timestamp_ms = self.clock.now();
        let mut started_at_ms = None;
        let result = match session
            .extensions
            .emit_user_bash(&event, &extension_context)
            .await
        {
            Some(result) => {
                started_at_ms = Some(self.clock.now());
                result
            }
            None => {
                BashExecutor::execute(
                    BashExecutionRequest::builder()
                        .command(input.command.clone())
                        .cwd(session.cwd.clone())
                        .session_id(
                            extension_context.invocation.session_id.clone(),
                        )
                        .turn_id(turn_id.clone())
                        .cancellation(cancellation)
                        .build(),
                    |snapshot| {
                        if started_at_ms.is_none()
                            && !snapshot.output.is_empty()
                        {
                            started_at_ms = Some(self.clock.now());
                        }
                    },
                )
                .await?
                .result
            }
        };
        let ended_at_ms = self.clock.now();
        let message = AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id()?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((
                timestamp_ms,
                started_at_ms.unwrap_or(ended_at_ms),
                ended_at_ms,
            ))
            .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::BashExecution {
                bash: BashExecutionMessage::builder()
                    .command(input.command)
                    .result(result.clone())
                    .exclude_from_context(input.exclude_from_context)
                    .build(),
            },
        };
        self.persist_message(&session, &message)?;
        // MessageEnd acknowledges the completed Bash transcript entry.
        session.sync_store()?;
        EventEmitter {
            clock: Arc::clone(&self.clock),
            sink,
            session: Arc::clone(&session),
        }
        .emit_at(
            turn_id,
            ended_at_ms,
            AgentEventPayload::MessageEnd {
                message: message.clone(),
            },
        )
        .await?;
        Ok(UserBashExecution {
            run_id,
            message,
            result,
        })
    }
}
