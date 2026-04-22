use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::RwLock;

use crate::{
    Result,
    context::{CompletedTurn, ContextManager, TurnContext},
    session::{SessionContinuationRequest, SessionId, ThreadId, Turn},
};
use llm::{completion::Message, usage::Usage};

#[derive(Debug, Default)]
struct SessionThreadState {
    history: ContextManager,
    continuations: Vec<SessionContinuationRequest>,
}

/// Stable session-scoped state used by turn/task execution.
#[derive(Debug, Default)]
pub struct SessionTaskContext {
    threads: RwLock<HashMap<(SessionId, ThreadId), SessionThreadState>>,
    fail_next_take_continuation: AtomicBool,
    fail_next_discard_turn: AtomicBool,
}

impl SessionTaskContext {
    /// Creates an empty session task context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forces the next continuation-drain attempt to fail for testing cleanup paths.
    pub fn fail_next_take_continuation(&self) {
        self.fail_next_take_continuation
            .store(true, Ordering::SeqCst);
    }

    /// Forces the next discard-turn attempt to fail for testing cleanup wrappers.
    pub fn fail_next_discard_turn(&self) {
        self.fail_next_discard_turn.store(true, Ordering::SeqCst);
    }

    /// Applies a mutation to one thread's history/context state.
    pub async fn with_history<R>(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        f: impl FnOnce(&mut ContextManager) -> R,
    ) -> R {
        let mut threads = self.threads.write().await;
        let thread = threads.entry((session_id, thread_id)).or_default();
        f(&mut thread.history)
    }

    /// Reads one thread's history/context state if the thread exists.
    pub async fn read_history<R>(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        f: impl FnOnce(&ContextManager) -> R,
    ) -> Option<R> {
        let threads = self.threads.read().await;
        threads
            .get(&(session_id, thread_id))
            .map(|thread| f(&thread.history))
    }

    /// Queues one continuation request for later task-level consumption.
    pub async fn queue_continuation(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        continuation: SessionContinuationRequest,
    ) {
        let mut threads = self.threads.write().await;
        let thread = threads.entry((session_id, thread_id)).or_default();
        thread.continuations.push(continuation);
    }

    /// Drains the oldest queued continuation request, if one exists.
    pub async fn drain_continuation(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Option<SessionContinuationRequest> {
        let mut threads = self.threads.write().await;
        let thread = threads.get_mut(&(session_id, thread_id))?;
        if thread.continuations.is_empty() {
            None
        } else {
            Some(thread.continuations.remove(0))
        }
    }

    /// Drains the next continuation request while preserving injected failure behavior.
    pub async fn take_continuation_state(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Result<Option<SessionContinuationRequest>> {
        if self
            .fail_next_take_continuation
            .swap(false, Ordering::SeqCst)
        {
            return Err(crate::Error::Runtime {
                message: "forced continuation failure".to_string(),
                stage: "test-continuation-failure".to_string(),
                inflight_snapshot: None,
            });
        }
        Ok(self.drain_continuation(session_id, thread_id).await)
    }

    /// Starts an active turn so incremental messages become visible immediately.
    pub async fn begin_turn_state(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        user_text: String,
        user_message: Message,
    ) -> Result<()> {
        self.with_history(session_id, thread_id, |history| {
            history.begin_turn(user_text, user_message);
        })
        .await;
        Ok(())
    }

    /// Appends one message to the active turn transcript.
    pub async fn append_message_state(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        message: Message,
    ) -> Result<()> {
        self.with_history(session_id, thread_id, |history| {
            history.append_message(message);
        })
        .await;
        Ok(())
    }

    /// Finalizes the active turn and advances the baseline snapshot.
    pub async fn finalize_turn_state(
        &self,
        turn_context: &TurnContext,
        usage: Usage,
    ) -> Result<()> {
        self.with_history(
            turn_context.session_id.clone(),
            turn_context.thread_id.clone(),
            |history| {
                history.finalize_turn(usage, turn_context);
            },
        )
        .await;
        Ok(())
    }

    /// Finalizes the active turn when only session/thread identifiers are available.
    pub async fn finalize_turn_by_id(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        usage: Usage,
    ) -> Result<()> {
        let turn_context = self
            .read_history(session_id.clone(), thread_id.clone(), |history| {
                history.reference_context_item()
            })
            .await
            .flatten()
            .map(|context_item| {
                let mut turn_context =
                    TurnContext::new(context_item.session_id, context_item.thread_id);
                turn_context.agent_id = context_item.agent_id;
                turn_context.parent_agent_id = context_item.parent_agent_id;
                turn_context.name = context_item.name;
                turn_context.system_prompt = context_item.system_prompt;
                turn_context.cwd = context_item.cwd;
                turn_context.current_date = context_item.current_date;
                turn_context.timezone = context_item.timezone;
                turn_context
            })
            .unwrap_or_else(|| TurnContext::new(session_id, thread_id));
        self.finalize_turn_state(&turn_context, usage).await
    }

    /// Discards the active turn after a failed execution.
    pub async fn discard_turn_state(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Result<()> {
        if self.fail_next_discard_turn.swap(false, Ordering::SeqCst) {
            return Err(crate::Error::Runtime {
                message: "forced discard failure".to_string(),
                stage: "test-discard-failure".to_string(),
                inflight_snapshot: None,
            });
        }
        self.with_history(session_id, thread_id, |history| {
            history.discard_turn();
        })
        .await;
        Ok(())
    }

    /// Queues a follow-up pending input for later task-level consumption.
    pub async fn queue_pending_input(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        input: String,
    ) -> Result<()> {
        self.queue_continuation(
            session_id,
            thread_id,
            SessionContinuationRequest::PendingInput { input },
        )
        .await;
        Ok(())
    }

    /// Drains the next pending-input continuation while leaving other continuation kinds untouched.
    pub async fn drain_pending_input(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Result<Option<String>> {
        let mut threads = self.threads.write().await;
        let Some(thread) = threads.get_mut(&(session_id, thread_id)) else {
            return Ok(None);
        };
        let Some(continuation) = thread.continuations.first() else {
            return Ok(None);
        };

        match continuation {
            SessionContinuationRequest::PendingInput { .. } => {
                let Some(SessionContinuationRequest::PendingInput { input }) =
                    Some(thread.continuations.remove(0))
                else {
                    unreachable!("matched pending input should still be pending input");
                };
                Ok(Some(input))
            }
            SessionContinuationRequest::SystemFollowUp { .. } => Err(crate::Error::Runtime {
                message: "take_pending_input cannot consume system follow-up continuation requests"
                    .to_string(),
                stage: "session-task-context-take-pending-input".to_string(),
                inflight_snapshot: None,
            }),
        }
    }

    /// Appends one completed turn reconstructed from legacy transcript storage.
    pub async fn append_turn_state(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        turn: Turn,
    ) -> Result<()> {
        self.with_history(session_id.clone(), thread_id.clone(), |history| {
            // Reconstructed turns do not carry a durable context snapshot yet, so
            // attach an empty baseline placeholder until the runtime migration
            // routes real TurnContextItem values through this path.
            history.append_turn(CompletedTurn {
                user_text: turn.user_text,
                transcript: turn.transcript,
                usage: turn.usage,
                context_item: TurnContext::new(session_id.clone(), thread_id.clone())
                    .to_turn_context_item(),
            });
        })
        .await;
        Ok(())
    }

    /// Loads the most recent prompt-visible messages for one thread.
    pub async fn load_messages_state(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<Message>> {
        Ok(self
            .read_history(session_id, thread_id, |history| {
                history.prompt_messages(limit)
            })
            .await
            .unwrap_or_default())
    }
}
