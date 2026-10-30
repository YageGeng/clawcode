use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use protocol::{
    AgentEventPayload, ContentBlock, IdKind, InputResult, MessageContent,
    QueueId, RunRequest, RunResult, SessionId, TraceId, TurnId,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::queue::{QueueRequest, QueueWaiter};
use super::{
    EventEmitter, EventSink, InsertionNotice, Kernel, KernelError,
    RunInvocation, SessionExecution, WhenBusy,
};

/// Admission counters and abandoned positions share one synchronous lock.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct AdmissionState {
    issued: u64,
    next: watch::Sender<u64>,
    released: BTreeSet<u64>,
}

impl Default for AdmissionState {
    /// Starts a Session's admission stream at position zero.
    fn default() -> Self {
        Self {
            issued: 0,
            next: watch::channel(0).0,
            released: BTreeSet::new(),
        }
    }
}

/// One prompt's original arrival position and cancellation generation.
#[derive(typed_builder::TypedBuilder)]
pub struct AdmissionTicket {
    execution: Arc<SessionExecution>,
    number: u64,
    pub(super) cancellation: CancellationToken,
    released: Mutex<bool>,
}

impl AdmissionTicket {
    /// Waits for earlier admissions, including tickets dropped before their turn.
    async fn wait(&self) -> Result<(), KernelError> {
        let mut next = self
            .execution
            .admission
            .lock()
            .map_err(|_error| KernelError::Poisoned)?
            .next
            .subscribe();
        loop {
            if self.cancellation.is_cancelled() {
                return Err(KernelError::PromptCancelled);
            }
            if *next.borrow_and_update() == self.number {
                return Ok(());
            }
            tokio::select! {
                biased;
                () = self.cancellation.cancelled() => return Err(KernelError::PromptCancelled),
                changed = next.changed() => changed.map_err(|_error| KernelError::SessionClosing)?,
            }
        }
    }

    /// Releases admission at queueing, handling, or operation-gate acquisition, exactly once.
    pub(super) fn release(&self) {
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *released {
            return;
        }
        *released = true;
        let mut state = self
            .execution
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.released.insert(self.number);
        let mut next = *state.next.borrow();
        while state.released.remove(&next) {
            next += 1;
        }
        state.next.send_replace(next);
    }
}

impl Drop for AdmissionTicket {
    /// Abandoned requests must not leave a hole in the admission stream.
    fn drop(&mut self) {
        self.release();
    }
}

/// Client input with its ordering, tracing, and insertion response ownership.
#[derive(typed_builder::TypedBuilder)]
pub struct PromptRequest {
    pub run: RunRequest,
    pub sink: Arc<dyn EventSink>,
    pub trace_id: TraceId,
    pub insertion: Arc<InsertionNotice>,
    pub ticket: AdmissionTicket,
    #[builder(default)]
    pub when_busy: Option<WhenBusy>,
    #[builder(default)]
    pub submission_id: Option<String>,
}

/// Admission result; the insertion notice independently completes the protocol request.
pub enum PromptOutcome {
    Ran(RunResult),
    Queued(QueueId),
    Handled,
}

impl Kernel {
    /// Allocates a position synchronously before protocol dispatch can reorder tasks.
    pub fn admission_ticket(
        &self,
        session_id: &SessionId,
    ) -> Result<AdmissionTicket, KernelError> {
        let session = self.session(session_id)?;
        let cancellation = session.execution.waiter_token()?;
        let mut state = session
            .execution
            .admission
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        let number = state.issued;
        state.issued = state.issued.checked_add(1).ok_or_else(|| {
            KernelError::Protocol("admission sequence overflow".into())
        })?;
        Ok(AdmissionTicket::builder()
            .execution(Arc::clone(&session.execution))
            .number(number)
            .cancellation(cancellation)
            .released(Mutex::new(false))
            .build())
    }

    /// Applies one input hook in arrival order, then queues or acquires the operation gate.
    pub async fn prompt(
        &self,
        request: PromptRequest,
    ) -> Result<PromptOutcome, KernelError> {
        let notice = Arc::clone(&request.insertion);
        let result = self.admit_prompt(request).await;
        if let Err(error) = &result {
            notice.rejected(error);
        }
        result
    }

    /// Keeps the admission ticket alive until queue publication or gate acquisition.
    async fn admit_prompt(
        &self,
        mut request: PromptRequest,
    ) -> Result<PromptOutcome, KernelError> {
        request.ticket.wait().await?;
        let session = self.session(&request.run.session_id)?;
        session.transcript.ensure_writable()?;
        if !Arc::ptr_eq(&session.execution, &request.ticket.execution) {
            return Err(KernelError::Protocol(
                "admission ticket belongs to another session".into(),
            ));
        }
        let placement = request.when_busy.unwrap_or(self.when_busy);
        let direct = if let Some(text) = request.run.input.slash_command_text()
        {
            if protocol::UserBashInput::parse_prefixed(text).is_some() {
                true
            } else if let Ok(command) =
                protocol::SlashCommandInvocation::try_from(text)
            {
                !matches!(
                    session.resolve_direct_slash_command(&command)?,
                    super::command::DirectSlashCommandResolution::NotFound
                )
            } else {
                false
            }
        } else {
            false
        };
        if !direct {
            if let protocol::RunInput::Text(text) = &request.run.input {
                let context = self.extension_context(&session, None, None)?;
                let result = session
                    .extensions
                    .runtime_ref()
                    .emit_input(
                        protocol::InputEvent {
                            text: text.clone(),
                            source: protocol::InputSource::Rpc,
                            streaming_behavior: Some(match placement {
                                WhenBusy::Steer => {
                                    protocol::InputStreamingBehavior::Steer
                                }
                                WhenBusy::FollowUp => {
                                    protocol::InputStreamingBehavior::FollowUp
                                }
                            }),
                        },
                        &context,
                    )
                    .await;
                if request.ticket.cancellation.is_cancelled() {
                    return Err(KernelError::PromptCancelled);
                }
                match result {
                    InputResult::Continue => {}
                    InputResult::Transform { text } => {
                        request.run.input = text.into()
                    }
                    InputResult::Handled => {
                        let turn = TurnId::try_from(
                            self.id_generator.next(IdKind::Turn),
                        )
                        .map_err(|e| KernelError::Protocol(e.to_string()))?;
                        let emitter = EventEmitter {
                            clock: Arc::clone(&self.clock),
                            sink: Arc::clone(&request.sink),
                            session: Arc::clone(&session),
                        };
                        self.insert_handled(
                            &emitter,
                            &turn,
                            text.clone(),
                            &request.insertion,
                            &request.ticket.cancellation,
                        )
                        .await?;
                        return Ok(PromptOutcome::Handled);
                    }
                }
            }
            if request.ticket.cancellation.is_cancelled() {
                return Err(KernelError::PromptCancelled);
            }
            match self
                .enqueue(
                    &request.run.session_id,
                    QueueRequest::builder()
                        .kind(placement.into())
                        .input(request.run.input.clone())
                        .waiter(Some(QueueWaiter::Insertion(Arc::clone(
                            &request.insertion,
                        ))))
                        .submission_id(request.submission_id)
                        .input_processed(true)
                        .cancellation(Some(request.ticket.cancellation.clone()))
                        .build(),
                )
                .await
            {
                Ok(queued) => {
                    tracing::info!(
                        "admitted prompt {} for session {} into queue {}",
                        request.ticket.number,
                        request.run.session_id,
                        queued.queue_id
                    );
                    return Ok(PromptOutcome::Queued(queued.queue_id));
                }
                Err(KernelError::SessionNotRunning(_)) => {}
                Err(error) => return Err(error),
            }
        }
        tracing::debug!(
            "admitting prompt {} for session {} through the operation gate",
            request.ticket.number,
            request.run.session_id
        );
        let result = self
            .run_with_source(
                request.run,
                request.sink,
                RunInvocation::builder()
                    .input_source(protocol::InputSource::Rpc)
                    .trace_id(request.trace_id)
                    .insertion(Some(request.insertion))
                    .input_processed(!direct)
                    .admission(Some(request.ticket))
                    .build(),
            )
            .await?;
        Ok(PromptOutcome::Ran(result.0))
    }

    /// Accepts handled input as a live-only message without starting a Run or writing history.
    pub(super) async fn insert_handled(
        &self,
        emitter: &EventEmitter,
        turn: &TurnId,
        text: String,
        notice: &InsertionNotice,
        cancellation: &CancellationToken,
    ) -> Result<(), KernelError> {
        emitter.session.sync_store()?;
        let message = self.message_now(
            turn,
            MessageContent::User {
                blocks: vec![ContentBlock::Text { text }],
            },
        )?;
        emitter
            .emit(
                turn.clone(),
                AgentEventPayload::MessageStart {
                    message_id: message.identity.message_id.clone(),
                },
            )
            .await?;
        if cancellation.is_cancelled() {
            return Err(KernelError::PromptCancelled);
        }
        emitter
            .emit(
                turn.clone(),
                AgentEventPayload::MessageEnd {
                    message: message.clone(),
                },
            )
            .await?;
        notice.inserted(&message.identity.message_id);
        Ok(())
    }
}
