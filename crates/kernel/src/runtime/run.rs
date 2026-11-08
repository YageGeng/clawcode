//! From input to settlement: admission, queueing, the Turn loop, and its outcome.

pub(super) mod admission;
mod attempt;
mod input;
pub(super) mod insertion;
pub(super) mod lifecycle;
pub(super) mod queue;
pub(super) mod retry;
mod settlement;
pub(super) mod submit;
pub(super) mod trace;
mod turn;

use std::sync::Arc;

use protocol::{
    AgentEventPayload, AgentMessage, AgentOutcome, IdKind, MessageContent,
    MessageIdentity, MessageTiming, RunId, RunRequest, RunResult, SessionId,
    TraceId, TurnId,
};
use store::RecordKind;
use tokio_util::sync::CancellationToken;
use turn::{RunScope, TurnLoop};

use crate::EventSink;
use crate::runtime::{EventEmitter, Kernel, KernelError, Session, command};
use admission::AdmissionTicket;
use input::PromptInputExpansion;
use insertion::{InsertionNotice, SubmissionAnswer, WaiterCancellation};
use lifecycle::ActiveRunContext;
use retry::RetryState;
use settlement::RunSettlement;
use trace::{RunSpan, TraceOrigin};

impl Kernel {
    /// Executes one run with an explicit input source for trusted host callers.
    pub(in crate::runtime) async fn run_with_source(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
        invocation: RunInvocation,
    ) -> Result<(RunResult, Option<Arc<SubmissionAnswer>>), KernelError> {
        // Every path below is one Run: mint its identity and open its span
        // before any of them can log, so the whole operation shares one trace.
        // A minting failure is a pre-insertion error like any other.
        let result = async {
            let trace_id = invocation.trace.trace_id(&*self.id_generator)?;
            let run_id = self.new_run_id()?;
            // Keep the full generation state off every caller's async stack frame.
            RunSpan::top_level(&trace_id, &run_id)
                .instrument(Box::pin(self.run_until_settled(
                    request,
                    sink,
                    &invocation,
                    run_id,
                    &trace_id,
                )))
                .await
        }
        .await;
        // Every return path before insertion rejects the prompt; the notice
        // ignores this once the user message was already inserted.
        if let (Err(error), Some(notice)) =
            (&result, invocation.insertion.as_deref())
        {
            notice.rejected(error);
        }
        result
    }

    /// Runs every input path to settlement while reporting insertion to an optional notice.
    async fn run_until_settled(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
        invocation: &RunInvocation,
        run_id: RunId,
        trace_id: &TraceId,
    ) -> Result<(RunResult, Option<Arc<SubmissionAnswer>>), KernelError> {
        let input_source = invocation.input_source;
        let insertion = invocation.insertion.as_deref();
        // Pi handles `!` and `!!` before ordinary input hooks and agent Turns.
        if !invocation.input_processed
            && let Some(result) = self
                .try_execute_user_bash_request(
                    &request,
                    Arc::clone(&sink),
                    insertion,
                    invocation.admission.as_ref(),
                    &run_id,
                )
                .await?
        {
            return Ok((result, None));
        }

        // Resolve the Session before any command path can emit lifecycle events.
        let session = self.session(&request.session_id)?;
        let first_turn_id = self.new_turn_id()?;
        let operation_started_at = self.clock.now();

        // Direct Slash Commands share Run correlation but bypass the Agent
        // lifecycle and serialized Turn loop.
        if !invocation.input_processed
            && let Some(command_invocation) =
                request.input.slash_command_text().and_then(|input| {
                    protocol::SlashCommandInvocation::try_from(input).ok()
                })
            && let Some(result) = self
                .try_execute_direct_slash_command(
                    command::SlashCommandExecution::builder()
                        .session_id(&request.session_id)
                        .session(&session)
                        .run_id(&run_id)
                        .turn_id(&first_turn_id)
                        .sink(Arc::clone(&sink))
                        .started_at(operation_started_at)
                        .invocation(command_invocation)
                        .admission(invocation.admission.as_ref())
                        .insertion(insertion)
                        .build(),
                )
                .await?
        {
            return Ok((result, None));
        }

        // Install serialized Run state only after every immediate command path
        // has declined the request.
        let (_run_guard, cancellation) = session
            .acquire_operation_for_prompt(
                invocation.cancellation_parent.as_ref(),
                invocation.waiters(),
            )
            .await?;
        if let Some(ticket) = &invocation.admission {
            ticket.release();
        }
        tracing::info!(
            "started Kernel Run for session {} {}",
            request.session_id,
            invocation.origin_phrase()
        );
        let _active_run = session.execution.install_run(ActiveRunContext {
            run_id: run_id.clone(),
            turn_id: first_turn_id.clone(),
            sink: Arc::clone(&sink),
        })?;
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
        let scope = RunScope::builder()
            .kernel(self)
            .session_id(&request.session_id)
            .session(&session)
            .run_id(&run_id)
            .trace_id(trace_id)
            .emitter(&emitter)
            .cancellation(&cancellation)
            .input_source(input_source)
            .insertion(insertion)
            .build();

        let Some(run_input) = scope
            .transform_input(
                request.input,
                invocation.input_processed,
                &first_turn_id,
                &extension_context,
            )
            .await?
        else {
            return Ok((
                RunResult {
                    run_id,
                    messages: Vec::new(),
                    turns: Vec::new(),
                },
                None,
            ));
        };

        // RunStart marks the first externally visible Agent lifecycle event.
        emitter
            .emit(
                first_turn_id.clone(),
                AgentEventPayload::RunStart {
                    run_id: run_id.clone(),
                },
            )
            .await?;

        // Expansion may emit a diagnostic, but always preserves a complete
        // message payload for the first Turn.
        let expanded_input = match run_input {
            protocol::RunInput::Text(text) => {
                // Pi expands Skills before Templates after extensions transform input.
                session.expand_prompt_input(&request.session_id, &text)?
            }
            protocol::RunInput::Blocks(blocks) => {
                PromptInputExpansion::from_blocks(blocks)
            }
        };
        if let Some(diagnostic) = expanded_input.diagnostic().cloned() {
            emitter
                .emit(
                    first_turn_id.clone(),
                    AgentEventPayload::SkillDiagnostic { diagnostic },
                )
                .await?;
        }
        let prompt = expanded_input.text().to_string();
        let initial_message_content = expanded_input.message_content();
        let mut turn_loop = TurnLoop::builder()
            // Internal input has no client to return it to, so a cancellation
            // before insertion persists this copy instead of dropping it.
            .retained_input(
                insertion.is_none().then(|| initial_message_content.clone()),
            )
            .next_initial_input(initial_message_content)
            .next_turn_id(first_turn_id.clone())
            .retry_state(RetryState::new(
                &self.retry_policy,
                self.provider_retry_delay_limit_ms,
            ))
            .build();

        // Persist the Run boundary before Agent hooks or model work begin.
        self.record_operation(
            &session,
            &run_id,
            RecordKind::OperationStarted,
            serde_json::json!({ "operation": "agent" }),
        )?;

        session.execution.open_queue()?;
        let execution = scope
            .execute(&mut turn_loop, prompt, &extension_context)
            .await;
        scope.settle(execution, turn_loop, first_turn_id).await
    }
}

impl Kernel {
    /// Allocates and validates a fresh Run identifier.
    pub(super) fn new_run_id(&self) -> Result<RunId, KernelError> {
        RunId::try_from(self.id_generator.next(IdKind::Run))
            .map_err(|error| KernelError::Protocol(error.to_string()))
    }

    /// Opens the span of a top-level Run that starts a trace of its own.
    pub(super) fn new_trace_span(
        &self,
        run_id: &RunId,
    ) -> Result<RunSpan, KernelError> {
        let trace_id = TraceOrigin::New.trace_id(&*self.id_generator)?;
        Ok(RunSpan::top_level(&trace_id, run_id))
    }

    /// Allocates and validates a fresh Turn identifier.
    pub(super) fn new_turn_id(&self) -> Result<TurnId, KernelError> {
        TurnId::try_from(self.id_generator.next(IdKind::Turn))
            .map_err(|error| KernelError::Protocol(error.to_string()))
    }

    /// Materializes one message with a fresh identity at the current time.
    pub(super) fn message_now(
        &self,
        turn_id: &TurnId,
        content: MessageContent,
    ) -> Result<AgentMessage, KernelError> {
        let timestamp = self.clock.now();
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: self.message_id()?,
                turn_id: turn_id.clone(),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content,
        })
    }
}

/// Per-Run ingress context carried from the protocol adapter or host caller to settlement.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct RunInvocation {
    /// Origin reported to input hooks.
    input_source: protocol::InputSource,
    /// Where this Run's trace comes from; a new trace unless a caller supplies one.
    #[builder(default)]
    trace: TraceOrigin,
    /// Caller-owned cancellation that also stops this Run while it waits.
    #[builder(default)]
    cancellation_parent: Option<CancellationToken>,
    /// Client acceptance report; absent for internal input.
    #[builder(default)]
    insertion: Option<Arc<InsertionNotice>>,
    #[builder(default)]
    input_processed: bool,
    #[builder(default)]
    admission: Option<AdmissionTicket>,
}

impl RunInvocation {
    /// Describes where the Run came from for its start line.
    fn origin_phrase(&self) -> String {
        match &self.trace {
            TraceOrigin::New => format!("from {:?}", self.input_source),
            TraceOrigin::Inherit(_) => "as foreground subagent".to_string(),
            TraceOrigin::LinkFrom(parent) => {
                format!("as background subagent linked to trace {parent}")
            }
        }
    }

    /// Retains the ingress cancellation generation through every fallback gate wait.
    fn waiters(&self) -> WaiterCancellation {
        self.admission.as_ref().map_or_else(
            || WaiterCancellation::from(self.insertion.as_deref()),
            |ticket| WaiterCancellation::Captured(ticket.cancellation.clone()),
        )
    }
}

/// Internal input whose Run was cancelled before insertion and must stay in history.
#[derive(typed_builder::TypedBuilder)]
struct RetainedInput<'a> {
    session_id: &'a SessionId,
    session: &'a Arc<Session>,
    emitter: &'a EventEmitter,
    run_id: &'a RunId,
    turn_id: &'a TurnId,
    cancellation: &'a CancellationToken,
    input_source: protocol::InputSource,
    content: MessageContent,
}

impl RetainedInput<'_> {
    /// Persists the retained input, then settles its Run as cancelled.
    async fn settle(
        self,
        kernel: &Kernel,
    ) -> Result<(RunResult, Option<Arc<SubmissionAnswer>>), KernelError> {
        let message = kernel.message_now(self.turn_id, self.content)?;
        self.emitter
            .emit(
                self.turn_id.clone(),
                AgentEventPayload::MessageStart {
                    message_id: message.identity.message_id.clone(),
                },
            )
            .await?;
        kernel.persist_message(self.session, &message)?;
        self.emitter
            .emit(
                self.turn_id.clone(),
                AgentEventPayload::MessageEnd {
                    message: message.clone(),
                },
            )
            .await?;
        let answer = RunSettlement::builder()
            .kernel(kernel)
            .session_id(self.session_id)
            .session(self.session)
            .emitter(self.emitter)
            .run_id(self.run_id)
            .turn_id(self.turn_id.clone())
            .cancellation(self.cancellation)
            .outcome(&AgentOutcome::Cancelled)
            // Retained input ends before any model attempt runs.
            .usage(protocol::ModelUsage::default())
            .messages(vec![message.clone()])
            .build()
            .settle()
            .await?;
        tracing::info!(
            "persisted {:?} input after cancellation before insertion",
            self.input_source
        );
        Ok((
            RunResult {
                run_id: self.run_id.clone(),
                messages: vec![message],
                turns: Vec::new(),
            },
            Some(answer),
        ))
    }
}
