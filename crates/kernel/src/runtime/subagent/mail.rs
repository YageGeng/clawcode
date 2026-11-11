//! Inter-agent mailbox delivery: queue into a running recipient, write into
//! an idle one, or wake it for a new task.

use std::sync::Arc;

use protocol::{
    AgentEventPayload, AgentMail, AgentMessage, ExtensionMessage,
    MessageContent, RunInput, RunRequest, SessionId, TraceId, TurnId,
};
use tokio_util::sync::CancellationToken;

use crate::runtime::run::RunInvocation;
use crate::runtime::{EventEmitter, Kernel, KernelError, Session};

/// One inter-agent delivery request.
#[derive(typed_builder::TypedBuilder)]
pub struct MailDelivery {
    /// Session that receives the mail.
    pub recipient: SessionId,
    /// Mail to deliver; its payload is bounded before delivery.
    pub mail: AgentMail,
    /// Trace of the sending Run; a woken recipient Run links to it.
    #[builder(default)]
    pub link_trace: Option<TraceId>,
    /// Sender Turn token that aborts a wait for an idle recipient's gate.
    #[builder(default)]
    pub cancellation: Option<CancellationToken>,
}

/// Where one delivered mail landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailPlacement {
    /// Queued for the recipient's next model request.
    Queued,
    /// Written into an idle recipient without starting a Run.
    Written,
    /// Started a new recipient Run whose input is the mail.
    Woken,
}

impl Kernel {
    /// Queues mail into a running recipient, or writes it into an idle one and
    /// wakes it for a new task. Delivery never waits for the recipient's answer.
    pub async fn deliver_mail(
        &self,
        delivery: MailDelivery,
    ) -> Result<MailPlacement, KernelError> {
        let MailDelivery {
            recipient,
            mut mail,
            link_trace,
            cancellation,
        } = delivery;
        mail.payload = bounded_payload(mail.payload);
        let kind = mail.kind;
        let session = match self.session(&recipient) {
            Ok(session) => session,
            Err(KernelError::SessionNotFound(_)) => {
                // A persisted child receives mail after a restart: load it first.
                let header =
                    self.store_factory.header(&recipient)?.ok_or_else(
                        || KernelError::SessionNotFound(recipient.clone()),
                    )?;
                Box::pin(self.resume_session(recipient.clone(), header.cwd))
                    .await?;
                self.session(&recipient)?
            }
            Err(error) => return Err(error),
        };
        let author = mail.author.clone();
        let Some(mail) = session.execution.queue_mail(mail)? else {
            tracing::info!(
                "queued {} mail from {} for session {}",
                kind.as_str(),
                author,
                recipient
            );
            return Ok(MailPlacement::Queued);
        };
        if kind.triggers_turn() {
            self.wake_with_mail(&recipient, &session, mail, link_trace)?;
            tracing::info!(
                "woke session {} with {} mail from {}",
                recipient,
                kind.as_str(),
                author
            );
            return Ok(MailPlacement::Woken);
        }
        let _operation = match cancellation {
            Some(token) => tokio::select! {
                biased;
                () = token.cancelled() => return Err(KernelError::PromptCancelled),
                guard = session.acquire_operation() => guard?,
            },
            None => session.acquire_operation().await?,
        };
        let turn_id = self.new_turn_id()?;
        let emitter = EventEmitter {
            clock: Arc::clone(&self.clock),
            sink: self.session_sink(&recipient),
            session: Arc::clone(&session),
        };
        session
            .write_mail(self, &emitter, &turn_id, vec![mail])
            .await?;
        tracing::info!(
            "wrote {} mail from {} into idle session {}",
            kind.as_str(),
            author,
            recipient
        );
        Ok(MailPlacement::Written)
    }

    /// Starts a recipient Run whose first input is the mail, tracked by the
    /// recipient's own background work so closing it joins the Run.
    pub(in crate::runtime) fn wake_with_mail(
        &self,
        recipient: &SessionId,
        session: &Session,
        mail: AgentMail,
        link_trace: Option<TraceId>,
    ) -> Result<(), KernelError> {
        let kernel = self.clone();
        let session_id = recipient.clone();
        let ownership = session.ownership.clone();
        session.background.spawn(async move {
            let sink = kernel.session_sink(&session_id);
            let result = Box::pin(
                kernel.run_with_source(
                    RunRequest {
                        session_id: session_id.clone(),
                        input: RunInput::Mail(mail),
                    },
                    sink,
                    RunInvocation::builder()
                        .input_source(protocol::InputSource::Extension)
                        .link_trace(link_trace)
                        .build(),
                ),
            )
            .await;
            if let Err(error) = result {
                tracing::warn!(
                    "failed mail wake of session {}: {}",
                    session_id,
                    error
                );
                // A child whose first Run never started must not keep its slot.
                if let Some(edge) = ownership
                    && let Ok(owner) = kernel.session(&edge.session_id)
                    && let Err(mark) = owner
                        .roster
                        .mark_start_failed(&edge.name, error.to_string())
                {
                    tracing::warn!(
                        "failed to record start failure of {}: {}",
                        edge.name,
                        mark
                    );
                }
            }
        })
    }
}

impl Session {
    /// Persists mail as `agent_message` Extension messages in one Turn and
    /// emits each message lifecycle; returns the written messages in order.
    pub(in crate::runtime) async fn write_mail(
        &self,
        kernel: &Kernel,
        emitter: &EventEmitter,
        turn_id: &TurnId,
        mail: Vec<AgentMail>,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let mut written = Vec::with_capacity(mail.len());
        for mail in mail {
            let extension = ExtensionMessage::try_from(mail)
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
            let message = kernel.message_now(
                turn_id,
                MessageContent::Extension { extension },
            )?;
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::MessageStart {
                        message_id: message.identity.message_id.clone(),
                    },
                )
                .await?;
            kernel.persist_message(self, &message)?;
            emitter
                .emit(
                    turn_id.clone(),
                    AgentEventPayload::MessageEnd {
                        message: message.clone(),
                    },
                )
                .await?;
            written.push(message);
        }
        self.sync_store()?;
        Ok(written)
    }
}

/// Bounds one payload so no single mail can flood the recipient's context.
fn bounded_payload(payload: String) -> String {
    cut_bytes(payload, ::tools::TOOL_TEXT_BYTES, "\n\n[message truncated]")
}

/// Keeps `text` within `max_bytes`, `hint` included, cut on a UTF-8
/// boundary; the hint is appended only when anything was cut. Bytes only: a
/// line-based cut would drop a single oversized line entirely.
pub(super) fn cut_bytes(
    mut text: String,
    max_bytes: usize,
    hint: &str,
) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let end = text.floor_char_boundary(max_bytes.saturating_sub(hint.len()));
    text.truncate(end);
    text.push_str(hint);
    text
}
