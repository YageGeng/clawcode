use super::*;
use protocol::{QueueKind, RunInput};

/// Placement of delegated input that arrives during an active run.
#[derive(Debug, Clone, Copy)]
pub enum WhenBusy {
    Steer,
    FollowUp,
}

impl From<WhenBusy> for QueueKind {
    /// Maps delegated placement onto the existing durable queues.
    fn from(value: WhenBusy) -> Self {
        match value {
            WhenBusy::Steer => Self::Steering,
            WhenBusy::FollowUp => Self::FollowUp,
        }
    }
}

/// Structured Kernel input that cannot dispatch user bash or Slash Commands.
#[derive(typed_builder::TypedBuilder)]
pub struct SubmitRequest {
    /// Session that receives the input.
    pub session_id: SessionId,
    /// Ordered model-visible blocks that bypass text command expansion.
    pub content: Vec<ContentBlock>,
    /// Queue placement while another run is active.
    pub when_busy: WhenBusy,
    /// Parent Turn token; absent at a background boundary.
    #[builder(default)]
    pub cancellation: Option<CancellationToken>,
    /// Calling tool's Trace, or a new Trace when absent.
    #[builder(default)]
    pub trace_id: Option<TraceId>,
}

/// Terminal state of the run that answered delegated input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    Completed,
    Cancelled,
    Failed(String),
}

/// Last assistant answer, without duplicating the child Session's usage.
#[derive(Debug, Clone)]
pub struct SubmissionAnswer {
    /// Stable assistant identity used to deduplicate background reports.
    pub message_id: Option<MessageId>,
    /// Concatenated text blocks from the last assistant message.
    pub text: String,
    /// Outcome of the answering run.
    pub outcome: AnswerOutcome,
}

/// One in-memory submission receipt, settled exactly once by consumption or withdrawal.
#[derive(Debug)]
pub(super) struct SubmissionReceipt {
    sender: Mutex<Option<tokio::sync::oneshot::Sender<Arc<SubmissionAnswer>>>>,
}

impl SubmissionReceipt {
    /// Creates a receipt before enqueueing so even immediate consumption cannot lose its answer.
    pub(super) fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<Arc<SubmissionAnswer>>,
    ) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                sender: Mutex::new(Some(sender)),
            }),
            receiver,
        )
    }

    /// Completes a receipt once; a dropped caller does not invalidate the persisted run.
    pub(super) fn resolve(&self, answer: Arc<SubmissionAnswer>) {
        let mut sender = self.sender.lock().unwrap_or_else(|error| {
            tracing::error!(
                "submission receipt lock was poisoned; releasing its waiter"
            );
            error.into_inner()
        });
        if let Some(sender) = sender.take() {
            let _ = sender.send(answer);
        }
    }
}

impl SubmissionAnswer {
    /// Represents input withdrawn before an answering run can complete.
    pub(super) fn cancelled() -> Self {
        Self {
            message_id: None,
            text: String::new(),
            outcome: AnswerOutcome::Cancelled,
        }
    }
}

impl From<(&AgentOutcome, &[AgentMessage])> for SubmissionAnswer {
    /// Uses the final Run outcome and its own messages, never the Session's newest history.
    fn from((outcome, messages): (&AgentOutcome, &[AgentMessage])) -> Self {
        let message = messages.iter().rev().find(|message| {
            matches!(message.content, MessageContent::Assistant { .. })
        });
        Self {
            message_id: message
                .map(|message| message.identity.message_id.clone()),
            text: message
                .and_then(|message| message.content.text_content())
                .unwrap_or_default(),
            outcome: match outcome {
                AgentOutcome::Succeeded => AnswerOutcome::Completed,
                AgentOutcome::Cancelled => AnswerOutcome::Cancelled,
                AgentOutcome::Failed { message } => {
                    AnswerOutcome::Failed(message.clone())
                }
            },
        }
    }
}

impl Kernel {
    /// Installs one stable event router shared by all clones of this Kernel.
    pub fn install_event_sinks(
        &self,
        sinks: Arc<dyn crate::SessionEventSinks>,
    ) -> Result<(), KernelError> {
        self.event_sinks.set(sinks).map_err(|_lock_error| {
            tracing::warn!(
                "refused replacement of installed Session event sinks"
            );
            KernelError::EventSinksInstalled
        })
    }

    /// Uses the protocol adapter's projection or accepts events in headless operation.
    pub(in crate::runtime) fn session_sink(
        &self,
        session_id: &SessionId,
    ) -> Arc<dyn EventSink> {
        self.event_sinks.get().map_or_else(
            || Arc::new(crate::sink::DiscardEvents) as Arc<dyn EventSink>,
            |sinks| sinks.sink(session_id),
        )
    }

    /// Runs idle input or waits on the exact run that consumes its queued receipt.
    pub async fn submit(
        &self,
        request: SubmitRequest,
    ) -> Result<SubmissionAnswer, KernelError> {
        let session = self.session(&request.session_id)?;
        let trace_id = match request.trace_id {
            Some(id) => id,
            None => TraceId::try_from(self.id_generator.next(IdKind::Trace))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
        };
        let cancellation = request.cancellation.clone().unwrap_or_default();
        loop {
            if session.execution.is_idle()? {
                tracing::info!(
                    "started Submit for session {}",
                    request.session_id
                );
                let result = Box::pin(self.run_with_source(
                    RunRequest {
                        session_id: request.session_id.clone(),
                        input: RunInput::Blocks(request.content.clone()),
                    },
                    self.session_sink(&request.session_id),
                    protocol::InputSource::Extension,
                    trace_id.clone(),
                    request.cancellation.clone(),
                ))
                .await;
                match result {
                    Ok((_run, Some(answer))) => {
                        tracing::info!(
                            "completed Submit for session {}",
                            request.session_id
                        );
                        return Ok((*answer).clone());
                    }
                    Ok((_run, None)) => {
                        tracing::error!(
                            "structured Submit for session {} ended without an agent settlement",
                            request.session_id
                        );
                        return Err(KernelError::Protocol(
                            "structured submission has no agent settlement"
                                .into(),
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(
                            "failed Submit for session {}: {}",
                            request.session_id,
                            error
                        );
                        return Err(error);
                    }
                }
            }
            let (receipt, mut answer) = SubmissionReceipt::new();
            let queued = match self
                .queue_message_with_receipt(
                    &request.session_id,
                    request.when_busy.into(),
                    RunInput::Blocks(request.content.clone()),
                    Some(receipt),
                )
                .await
            {
                Ok(queued) => queued,
                Err(KernelError::SessionNotRunning(_)) => continue,
                Err(error) => return Err(error),
            };
            tracing::info!(
                "queued Submit {} for session {}",
                queued.queue_id,
                request.session_id
            );

            // A receipt completes before the execution gate is released. Later runs may
            // own that gate by the time this task wakes, but cannot replace its answer.
            let received = tokio::select! {
                biased;
                () = cancellation.cancelled() => None,
                result = &mut answer => Some(result),
                idle = session.execution.wait_for_idle() => {
                    idle?;
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => None,
                        result = &mut answer => Some(result),
                        operation = session.acquire_operation() => {
                            match operation {
                                Ok(guard) => {
                                    if session.execution.pending_ids()?.contains(&queued.queue_id) {
                                        self.remove_pending_message(&request.session_id, &queued.queue_id)?;
                                        tracing::info!("rescued stranded Submit {} for session {}", queued.queue_id, request.session_id);
                                        drop(guard);
                                        continue;
                                    }
                                    drop(guard);
                                    Some(answer.await)
                                }
                                Err(KernelError::SessionClosing) => {
                                    if session.execution.pending_ids()?.contains(&queued.queue_id) {
                                        let record = self.queue_cancellation_record(&session, None, queued.queue_id.clone(), QueueCancellationReason::Removed)?;
                                        session.remove_pending(record, &queued.queue_id)?;
                                    }
                                    return Ok(SubmissionAnswer::cancelled());
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                }
            };
            match received {
                Some(Ok(answer)) => {
                    tracing::info!(
                        "completed queued Submit {} for session {}",
                        queued.queue_id,
                        request.session_id
                    );
                    return Ok((*answer).clone());
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        "lost Submit receipt {} for session {}: {}",
                        queued.queue_id,
                        request.session_id,
                        error
                    );
                    return Err(KernelError::Protocol(
                        "submission receipt ended without settlement".into(),
                    ));
                }
                None => {
                    session.execution.cancel()?;
                    match self.clear_pending_messages(&request.session_id) {
                        Ok(())
                        | Err(
                            KernelError::SessionClosing
                            | KernelError::SessionNotFound(_),
                        ) => {}
                        Err(error) => return Err(error),
                    }
                    session.execution.wait_for_idle().await?;
                    return Ok(SubmissionAnswer::cancelled());
                }
            }
        }
    }
}
