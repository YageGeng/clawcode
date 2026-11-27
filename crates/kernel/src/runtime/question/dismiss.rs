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
        Ok(self.dismiss_question_keys(session_id, keys).await?.len())
    }

    /// Returns the exact keys accepted under the admission ticket so callers can distinguish
    /// skips from keys reserved by another client's removable answer.
    pub async fn dismiss_question_keys(
        &self,
        session_id: &SessionId,
        keys: Vec<QuestionKey>,
    ) -> Result<Vec<QuestionKey>, KernelError> {
        let session = self.session(session_id)?;
        session.transcript.ensure_writable()?;
        // The ticket orders this skip with answers; it is held until the skip
        // is visible to the next settlement.
        let ticket = self.admission_ticket(session_id)?;
        ticket.wait().await?;
        let requested = keys.len();
        let Some(mut skip) =
            QuestionLedger::try_from(session.as_ref())?.skip(keys)
        else {
            tracing::debug!(
                "none of {} questions requested for dismissal are open in session {}",
                requested,
                session_id
            );
            return Ok(Vec::new());
        };
        let keys: Vec<_> = skip.keys().collect();
        loop {
            // Registered before the queue check, so a Run opening its queue
            // in between still wakes this skip.
            let opened = session.execution.queue_opened();
            tokio::pin!(opened);
            opened.as_mut().enable();
            match session.execution.queue_skip(skip)? {
                None => {
                    // A held skip closes the question on every client before its durable message exists.
                    self.publish_queue_changed(session_id);
                    break;
                }
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
                    let extension = ExtensionMessage::try_from(&skip)
                        .map_err(|error| KernelError::Protocol(error.to_string()))?;
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
                            vec![extension],
                        )
                        .await?;
                    break;
                }
            }
        }
        tracing::info!(
            "dismissed {} of {} requested questions in session {}",
            keys.len(),
            requested,
            session_id
        );
        Ok(keys)
    }
}
