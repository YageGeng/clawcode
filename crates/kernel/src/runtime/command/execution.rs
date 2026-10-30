use super::super::*;
use super::{DirectSlashCommand, DirectSlashCommandResolution};

/// Correlation data shared by every directly handled Slash Command.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct SlashCommandExecution<'a> {
    pub(super) session_id: &'a SessionId,
    pub(super) session: &'a Arc<Session>,
    pub(super) run_id: &'a RunId,
    pub(super) turn_id: &'a TurnId,
    pub(super) trace_id: &'a TraceId,
    pub(super) sink: Arc<dyn EventSink>,
    pub(super) started_at: TimestampMs,
    pub(super) invocation: protocol::SlashCommandInvocation,
    /// Reports acceptance once the invocation message is durable.
    #[builder(default)]
    pub(super) insertion: Option<&'a InsertionNotice>,
}

impl SlashCommandExecution<'_> {
    /// Materializes the exact command input with this operation's stable identity.
    fn invocation_message(
        &self,
        kernel: &Kernel,
        source: protocol::SlashCommandSource,
    ) -> Result<AgentMessage, KernelError> {
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: kernel.message_id()?,
                turn_id: self.turn_id.clone(),
            },
            timing: MessageTiming::try_from((
                self.started_at,
                self.started_at,
                self.started_at,
            ))
            .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::SlashCommand {
                message: protocol::SlashCommandMessage::Invocation {
                    invocation: self.invocation.clone(),
                    source,
                },
            },
        })
    }

    /// Materializes one terminal command output at its actual completion time.
    fn output_message(
        &self,
        kernel: &Kernel,
        output: protocol::SlashCommandOutput,
        ended_at: TimestampMs,
    ) -> Result<AgentMessage, KernelError> {
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: kernel.message_id()?,
                turn_id: self.turn_id.clone(),
            },
            timing: MessageTiming::try_from((ended_at, ended_at, ended_at))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::SlashCommand {
                message: protocol::SlashCommandMessage::Output(output),
            },
        })
    }
}

impl Kernel {
    /// Executes a recognized direct command or preserves unrecognized input for the Agent.
    pub(in crate::runtime) async fn try_execute_direct_slash_command(
        &self,
        execution: SlashCommandExecution<'_>,
    ) -> Result<Option<RunResult>, KernelError> {
        let resolution = execution
            .session
            .resolve_direct_slash_command(&execution.invocation)?;
        let Some(source) = resolution.source() else {
            return Ok(None);
        };
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink: Arc::clone(&execution.sink),
            session: Arc::clone(execution.session),
        };
        let operation = match execution.session.try_acquire_operation()? {
            Some(operation) => operation,
            None => {
                let invocation_message =
                    execution.invocation_message(self, source)?;
                self.persist_and_emit_slash_message(
                    execution.session,
                    &emitter,
                    &invocation_message,
                )
                .await?;
                // A busy command is still inserted; its failure is the
                // persisted output while the running operation keeps the
                // foreground state.
                if let Some(notice) = execution.insertion {
                    notice.inserted(&invocation_message.identity.message_id);
                }
                let output_message = execution.output_message(
                    self,
                    protocol::SlashCommandOutput::builder()
                        .command(execution.invocation.name.clone())
                        .source(source)
                        .status(protocol::SlashCommandStatus::Failed)
                        .blocks(vec![ContentBlock::Text {
                            text: protocol::SlashCommandError::Busy {
                                command: execution.invocation.name.clone(),
                            }
                            .to_string(),
                        }])
                        .build(),
                    self.clock.now(),
                )?;
                self.persist_and_emit_slash_message(
                    execution.session,
                    &emitter,
                    &output_message,
                )
                .await?;
                tracing::warn!(
                    "rejected busy Slash Command /{} for session {}, Run {}, Turn {}, trace {}",
                    execution.invocation.name,
                    execution.session_id,
                    execution.run_id,
                    execution.turn_id,
                    execution.trace_id
                );
                return Ok(Some(RunResult {
                    run_id: execution.run_id.clone(),
                    messages: vec![invocation_message, output_message],
                    turns: Vec::new(),
                }));
            }
        };
        let cancellation =
            execution.session.execution.install_cancellation(None)?;
        tracing::info!(
            "started Slash Command /{} for session {}, Run {}, Turn {}, trace {}",
            execution.invocation.name,
            execution.session_id,
            execution.run_id,
            execution.turn_id,
            execution.trace_id
        );
        emitter
            .emit_at(
                execution.turn_id.clone(),
                execution.started_at,
                AgentEventPayload::SlashCommandStart {
                    run_id: execution.run_id.clone(),
                    invocation: execution.invocation.clone(),
                },
            )
            .await?;
        let persisted = async {
            let message = execution.invocation_message(self, source)?;
            self.persist_and_emit_slash_message(
                execution.session,
                &emitter,
                &message,
            )
            .await?;
            Ok::<_, KernelError>(message)
        }
        .await;
        let invocation_message = match persisted {
            Ok(message) => message,
            Err(error) => {
                // A started command must still reach a terminal state.
                if let Err(end_error) = emitter
                    .emit(
                        execution.turn_id.clone(),
                        AgentEventPayload::SlashCommandEnd {
                            run_id: execution.run_id.clone(),
                            status: protocol::SlashCommandStatus::Failed,
                            message: None,
                        },
                    )
                    .await
                {
                    tracing::warn!(
                        "failed to end Slash Command /{} for session {} after its invocation was lost: {}",
                        execution.invocation.name,
                        execution.session_id,
                        end_error
                    );
                }
                tracing::warn!(
                    "failed to persist Slash Command /{} invocation for session {}, Run {}: {}",
                    execution.invocation.name,
                    execution.session_id,
                    execution.run_id,
                    error
                );
                return Err(error);
            }
        };
        if let Some(notice) = execution.insertion {
            notice.inserted(&invocation_message.identity.message_id);
        }
        let outcome = match resolution {
            DirectSlashCommandResolution::Rejected { error, .. } => {
                Ok(protocol::SlashCommandOutcome::Rejected(error))
            }
            DirectSlashCommandResolution::Command(
                DirectSlashCommand::Builtin(command),
            ) => {
                let _operation = operation;
                self.execute_builtin_slash_command(
                    command,
                    &execution,
                    &emitter,
                    &cancellation,
                )
                .await
            }
            DirectSlashCommandResolution::Command(
                DirectSlashCommand::Extension(command),
            ) => {
                // Extension Host operations such as `send_user_message` and
                // `compact` acquire the same gate, so the command only uses
                // the non-blocking acquisition as an idle-state check.
                drop(operation);
                let context = self.extension_context_with_event_sink(
                    execution.session,
                    Some(execution.run_id),
                    Some(execution.turn_id),
                    Some(Arc::clone(&execution.sink)),
                );
                match context {
                    Ok(context) => match command
                        .handler
                        .handle(
                            &execution.invocation.arguments,
                            &serde_json::Value::Null,
                            &::extension::ExtensionCommandContext {
                                event: context
                                    .for_extension(&command.extension_id),
                            },
                        )
                        .await
                    {
                        Ok(()) => Ok(protocol::SlashCommandOutcome::Handled {
                            output: None,
                        }),
                        Err(error) => {
                            tracing::warn!(
                                "failed Extension Slash Command /{} for session {}, Run {}, Turn {}, trace {}: {}",
                                execution.invocation.name,
                                execution.session_id,
                                execution.run_id,
                                execution.turn_id,
                                execution.trace_id,
                                error
                            );
                            Ok(protocol::SlashCommandOutcome::Rejected(
                                protocol::SlashCommandError::ExecutionFailed {
                                    command: execution.invocation.name.clone(),
                                },
                            ))
                        }
                    },
                    Err(error) => Err(error),
                }
            }
            DirectSlashCommandResolution::NotFound => {
                return Err(KernelError::Protocol(
                    "resolved Slash Command disappeared during execution"
                        .to_string(),
                ));
            }
        };
        let (outcome, stopped) = match outcome {
            Ok(outcome) => (outcome, false),
            Err(error) => {
                tracing::warn!(
                    "failed Slash Command /{} for session {}, Run {}, Turn {}, trace {}: {}",
                    execution.invocation.name,
                    execution.session_id,
                    execution.run_id,
                    execution.turn_id,
                    execution.trace_id,
                    error
                );
                (
                    protocol::SlashCommandOutcome::Rejected(
                        protocol::SlashCommandError::ExecutionFailed {
                            command: execution.invocation.name.clone(),
                        },
                    ),
                    error.is_cancellation(),
                )
            }
        };
        let (mut output, mut status) = match outcome {
            protocol::SlashCommandOutcome::Handled { output } => {
                let status = output.as_ref().map_or(
                    protocol::SlashCommandStatus::Succeeded,
                    |output| output.status,
                );
                (output, status)
            }
            protocol::SlashCommandOutcome::Rejected(error) => (
                Some(
                    protocol::SlashCommandOutput::builder()
                        .command(execution.invocation.name.clone())
                        .source(source)
                        .status(protocol::SlashCommandStatus::Failed)
                        .blocks(vec![ContentBlock::Text {
                            text: error.to_string(),
                        }])
                        .build(),
                ),
                protocol::SlashCommandStatus::Failed,
            ),
            protocol::SlashCommandOutcome::ExpandedPrompt(_)
            | protocol::SlashCommandOutcome::NotFound => {
                return Err(KernelError::Protocol(
                    "direct Slash Command returned a non-direct outcome"
                        .to_string(),
                ));
            }
        };
        // A command stopped by cancel or close settles as cancelled, not failed.
        if status == protocol::SlashCommandStatus::Failed
            && (stopped || cancellation.is_cancelled())
        {
            status = protocol::SlashCommandStatus::Cancelled;
            output = Some(
                protocol::SlashCommandOutput::builder()
                    .command(execution.invocation.name.clone())
                    .source(source)
                    .status(protocol::SlashCommandStatus::Cancelled)
                    .blocks(vec![ContentBlock::Text {
                        text: format!(
                            "/{} was cancelled",
                            execution.invocation.name
                        ),
                    }])
                    .build(),
            );
        }
        // The failure summary is the user-facing output text, never an internal error.
        let failure = (status == protocol::SlashCommandStatus::Failed)
            .then(|| {
                output.as_ref().and_then(|output| {
                    output.blocks.iter().find_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                })
            })
            .flatten();
        let mut messages = vec![invocation_message];
        if let Some(output) = output {
            let output_message =
                execution.output_message(self, output, self.clock.now())?;
            self.persist_and_emit_slash_message(
                execution.session,
                &emitter,
                &output_message,
            )
            .await?;
            messages.push(output_message);
        }
        // SlashCommandEnd is the externally visible operation boundary for
        // command-owned writes such as manual compaction.
        execution.session.sync_store()?;
        emitter
            .emit(
                execution.turn_id.clone(),
                AgentEventPayload::SlashCommandEnd {
                    run_id: execution.run_id.clone(),
                    status,
                    message: failure,
                },
            )
            .await?;
        tracing::info!(
            "settled Slash Command /{} for session {}, Run {}, Turn {} with status {:?}",
            execution.invocation.name,
            execution.session_id,
            execution.run_id,
            execution.turn_id,
            status
        );
        Ok(Some(RunResult {
            run_id: execution.run_id.clone(),
            messages,
            turns: Vec::new(),
        }))
    }

    /// Persists one command message and emits its complete message lifecycle.
    async fn persist_and_emit_slash_message(
        &self,
        session: &Session,
        emitter: &EventEmitter,
        message: &AgentMessage,
    ) -> Result<(), KernelError> {
        emitter
            .emit_at(
                message.identity.turn_id.clone(),
                message.timing.timestamp_ms,
                AgentEventPayload::MessageStart {
                    message_id: message.identity.message_id.clone(),
                },
            )
            .await?;
        self.persist_message(session, message)?;
        session.sync_store()?;
        emitter
            .emit_at(
                message.identity.turn_id.clone(),
                message.timing.ended_at_ms,
                AgentEventPayload::MessageEnd {
                    message: message.clone(),
                },
            )
            .await
    }
}
