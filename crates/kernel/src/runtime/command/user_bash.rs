use std::sync::Arc;

use ::tools::{BashExecutionRequest, BashExecutor};
use protocol::{
    AgentEventPayload, AgentMessage, AgentOutcome, BashExecutionMessage,
    MessageContent, MessageId, MessageIdentity, MessageTiming, RunId,
    RunRequest, RunResult, SessionId, TimestampMs, TurnId, UserBashInput,
    UserBashRequest, UserBashResult,
};
use tokio_util::sync::CancellationToken;

use crate::EventSink;
use crate::runtime::run::admission::AdmissionTicket;
use crate::runtime::run::insertion::{InsertionNotice, WaiterCancellation};
use crate::runtime::run::lifecycle::ActiveRunContext;
use crate::runtime::{EventEmitter, Kernel, KernelError, Session};

/// Internal correlated result shared by prefixed prompts and direct ACP requests.
pub(super) struct UserBashExecution {
    pub(super) run_id: RunId,
    pub(super) message: AgentMessage,
    result: UserBashResult,
}

/// Inputs of one user-bash Run, from the `!` prompt path or a server-side request.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct UserBashRun<'a> {
    session_id: &'a SessionId,
    session: Arc<Session>,
    input: UserBashInput,
    sink: Arc<dyn EventSink>,
    /// Minted by the caller, which already opened this Run's span.
    run_id: RunId,
    /// Client acceptance report; absent for server-side requests.
    #[builder(default)]
    insertion: Option<&'a InsertionNotice>,
    /// Admission ticket released once the shell owns the Session gate.
    #[builder(default)]
    admission: Option<&'a AdmissionTicket>,
}

impl Kernel {
    /// Routes a prefixed run request into user-bash before ordinary input hooks.
    pub(in crate::runtime) async fn try_execute_user_bash_request(
        &self,
        request: &RunRequest,
        sink: Arc<dyn EventSink>,
        insertion: Option<&InsertionNotice>,
        admission: Option<&AdmissionTicket>,
        run_id: &RunId,
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
                UserBashRun::builder()
                    .session_id(&request.session_id)
                    .session(self.session(&request.session_id)?)
                    .input(input)
                    .sink(sink)
                    .run_id(run_id.clone())
                    .insertion(insertion)
                    .admission(admission)
                    .build(),
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
        // A server-side shell request is a top-level Run with a trace of its own.
        let run_id = self.new_run_id()?;
        let execution = self
            .new_trace_span(&run_id)?
            .instrument(
                self.execute_user_bash_operation(
                    UserBashRun::builder()
                        .session_id(&request.session_id)
                        .session(session)
                        .input(UserBashInput {
                            command: request.command,
                            exclude_from_context: request.exclude_from_context,
                        })
                        .sink(sink)
                        .run_id(run_id.clone())
                        .build(),
                ),
            )
            .await?;
        Ok(execution.result)
    }

    /// Serializes one bash operation, runs its extension hook, and materializes its message.
    pub(super) async fn execute_user_bash_operation(
        &self,
        run: UserBashRun<'_>,
    ) -> Result<UserBashExecution, KernelError> {
        let UserBashRun {
            session_id,
            session,
            input,
            sink,
            run_id,
            insertion,
            admission,
        } = run;
        // A cancel or close while this shell waits for the gate must reject it
        // before any process is spawned.
        let (_run_guard, cancellation) = session
            .acquire_operation_for_prompt(
                None,
                admission.map_or_else(
                    || WaiterCancellation::from(insertion),
                    |ticket| {
                        WaiterCancellation::Captured(
                            ticket.cancellation.clone(),
                        )
                    },
                ),
            )
            .await?;
        if let Some(ticket) = admission {
            ticket.release();
        }
        // The caller minted this Run's id and already opened its span.
        let turn_id = self.new_turn_id()?;
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
            "started user bash for session {} with message {}",
            session_id,
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
                tracing::warn!("user bash failed after acceptance: {}", error);
                (
                    AgentOutcome::Failed {
                        message: error.to_string(),
                    },
                    false,
                )
            }
        };
        tracing::info!("finished user bash with outcome {:?}", outcome);
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
                                "failed to report user bash output: {}",
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
