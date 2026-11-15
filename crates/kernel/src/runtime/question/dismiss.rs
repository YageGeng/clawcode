//! Skips: settle open questions without an answer and without starting a Run.

use std::sync::Arc;

use protocol::{ExtensionMessage, QuestionKey, SessionId};

use super::QuestionLedger;
use crate::runtime::{EventEmitter, Kernel, KernelError};

impl Kernel {
    /// Skips the still-open questions among `keys` and returns how many it
    /// skipped. A running Session writes the skip before its next model
    /// request or at Run close, like queue-only mail; an idle one gets it at
    /// once. Neither starts or extends a Run.
    pub async fn dismiss_questions(
        &self,
        session_id: &SessionId,
        keys: Vec<QuestionKey>,
    ) -> Result<usize, KernelError> {
        let session = self.session(session_id)?;
        session.transcript.ensure_writable()?;
        // The ticket orders this skip with answers; it is held until the skip
        // is visible to the next settlement.
        let ticket = self.admission_ticket(session_id)?;
        ticket.wait().await?;
        let Some(reply) =
            QuestionLedger::try_from(session.as_ref())?.skip(keys)
        else {
            return Ok(0);
        };
        let mut skip = ExtensionMessage::try_from(&reply)
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        loop {
            // Registered before the queue check, so a Run opening its queue
            // in between still wakes this skip.
            let opened = session.execution.queue_opened();
            tokio::pin!(opened);
            opened.as_mut().enable();
            match session.execution.queue_skip(skip)? {
                None => break,
                Some(returned) => skip = returned,
            }
            // A closed queue means an idle Session, another operation, or a
            // Run starting or settling. The skip joins a Run that opens its
            // queue, or is written once the gate frees; never waiting on the
            // gate alone keeps a starting Run from stalling it.
            tokio::select! {
                biased;
                () = ticket.cancellation.cancelled() => {
                    return Err(KernelError::PromptCancelled);
                }
                () = &mut opened => {}
                operation = session.acquire_operation() => {
                    let _operation = operation?;
                    let emitter = EventEmitter {
                        clock: Arc::clone(&self.clock),
                        sink: self.session_sink(session_id),
                        session: Arc::clone(&session),
                    };
                    session
                        .write_extensions(
                            self,
                            &emitter,
                            &self.new_turn_id()?,
                            vec![skip],
                        )
                        .await?;
                    break;
                }
            }
        }
        tracing::info!(
            "dismissed {} questions in session {}",
            reply.items.len(),
            session_id
        );
        Ok(reply.items.len())
    }
}
