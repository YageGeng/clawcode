use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::RwLock;

use crate::{
    Result,
    context::{CompletedTurn, ContextManager, TurnContext, TurnContextItem},
    session::{SessionContinuationRequest, SessionId, ThreadId, Turn},
};
use llm::{completion::Message, usage::Usage};
use store::SessionStore;

#[derive(Debug, Default)]
struct SessionThreadState {
    history: ContextManager,
    continuations: Vec<SessionContinuationRequest>,
    cached_system_prompt: Option<String>,
}

/// Stable session-scoped state used by turn/task execution.
///
/// When constructed with a [`SessionStore`], every turn lifecycle event
/// is persisted to the configured backend. Persistence failures are logged
/// and do not interrupt the agent loop.
#[derive(Default)]
pub struct SessionTaskContext {
    threads: RwLock<HashMap<(SessionId, ThreadId), SessionThreadState>>,
    persistence: Option<Arc<dyn SessionStore>>,
    fail_next_take_continuation: AtomicBool,
    fail_next_discard_turn: AtomicBool,
}

impl fmt::Debug for SessionTaskContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTaskContext")
            .field(
                "persistence",
                &self.persistence.as_ref().map(|_| "<SessionStore>"),
            )
            .finish_non_exhaustive()
    }
}

impl SessionTaskContext {
    /// Creates an empty session task context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a persistence backend. All subsequent turn events are
    /// recorded to the store.
    pub fn with_persistence(mut self, persistence: Arc<dyn SessionStore>) -> Self {
        self.persistence = Some(persistence);
        self
    }

    /// Returns the attached persistence backend, if any.
    pub fn persistence(&self) -> Option<&Arc<dyn SessionStore>> {
        self.persistence.as_ref()
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

    /// Returns the cached system prompt for one thread when it is still valid.
    pub async fn read_cached_system_prompt(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Option<String> {
        let threads = self.threads.read().await;
        threads
            .get(&(session_id, thread_id))
            .and_then(|thread| thread.cached_system_prompt.clone())
    }

    /// Stores one rebuilt system prompt for reuse by later turns on the same thread.
    pub async fn save_cached_system_prompt(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        system_prompt: String,
    ) {
        let mut threads = self.threads.write().await;
        let thread = threads.entry((session_id, thread_id)).or_default();

        thread.cached_system_prompt = Some(system_prompt);
    }

    /// Seeds a durable turn context baseline for a thread that has not produced any turns yet.
    pub async fn seed_turn_context(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        turn_context: TurnContext,
    ) {
        self.with_history(session_id, thread_id, |history| {
            history.set_reference_context_item(Some(turn_context.to_turn_context_item()));
        })
        .await;
    }

    /// Loads the latest durable turn context snapshot for one thread when it exists.
    pub async fn load_turn_context(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Option<TurnContext> {
        self.read_history(session_id, thread_id, |history| {
            history.reference_context_item().map(TurnContext::from_item)
        })
        .await
        .flatten()
    }

    /// Invalidates the cached system prompt so the next turn must rebuild it.
    pub async fn expire_system_prompt(&self, session_id: SessionId, thread_id: ThreadId) {
        let mut threads = self.threads.write().await;
        if let Some(thread) = threads.get_mut(&(session_id, thread_id)) {
            thread.cached_system_prompt = None;
        }
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
        if let Some(p) = &self.persistence {
            p.record_turn_started(
                session_id.as_uuid(),
                thread_id.as_uuid(),
                &user_text,
                &user_message,
            )
            .await;
        }
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
        if let Some(p) = &self.persistence {
            p.record_message(session_id.as_uuid(), thread_id.as_uuid(), &message)
                .await;
        }
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
        if let Some(p) = &self.persistence {
            let context_item = serde_json::to_value(turn_context.to_turn_context_item())
                .unwrap_or_else(|e| {
                    tracing::warn!("failed to serialize turn context for persistence: {e}");
                    serde_json::Value::Null
                });
            p.record_turn_completed(
                turn_context.session_id.as_uuid(),
                turn_context.thread_id.as_uuid(),
                usage,
                context_item,
            )
            .await;
        }
        self.with_history(
            turn_context.session_id,
            turn_context.thread_id.clone(),
            |history| {
                history.finalize_turn(usage, turn_context);
            },
        )
        .await;
        Ok(())
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
        if let Some(p) = &self.persistence {
            p.record_turn_discarded(session_id.as_uuid(), thread_id.as_uuid())
                .await;
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
        self.with_history(session_id, thread_id.clone(), |history| {
            // Reconstructed turns do not carry a durable context snapshot yet, so
            // attach an empty baseline placeholder until the runtime migration
            // routes real TurnContextItem values through this path.
            history.append_turn(CompletedTurn {
                user_text: turn.user_text,
                transcript: turn.transcript,
                usage: turn.usage,
                context_item: TurnContext::new(session_id, thread_id.clone())
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

    /// Replays persisted turn events into this store, restoring session state.
    ///
    /// Uses internal history operations to avoid triggering persistence hooks
    /// during replay. Returns the `(session_id, thread_id)` from the first
    /// `TurnStarted` event.
    ///
    /// Corrupted events (e.g. `TurnCompleted` without a preceding `TurnStarted`)
    /// are logged and skipped rather than panicking.
    pub async fn load_from_events(
        &self,
        events: Vec<store::SessionEvent>,
    ) -> Result<(SessionId, ThreadId)> {
        use store::SessionEvent;

        if events.is_empty() {
            return Err(crate::Error::Runtime {
                message: "session replay received an empty event list".to_string(),
                stage: "session-load-from-events".to_string(),
                inflight_snapshot: None,
            });
        }

        let mut session_id: Option<SessionId> = None;
        let mut thread_id: Option<ThreadId> = None;
        // Track active turns per replayed thread so interleaved parent/child events
        // can finalize independently without clobbering each other's lifecycle state.
        let mut active_turns = HashSet::new();

        for event in events {
            match event {
                SessionEvent::TurnStarted {
                    session_id: sid,
                    thread_id: tid,
                    user_text,
                    message,
                    ..
                } => {
                    if session_id.is_none() {
                        session_id = Some(SessionId::from(sid));
                        thread_id = Some(ThreadId::from(tid));
                    }
                    let sid = SessionId::from(sid);
                    let tid = ThreadId::from(tid);
                    let replay_key = (sid, tid.clone());
                    self.with_history(sid, tid, |history| {
                        history.begin_turn(user_text, message);
                    })
                    .await;
                    active_turns.insert(replay_key);
                }
                SessionEvent::Message {
                    session_id: sid,
                    thread_id: tid,
                    message,
                    ..
                } => {
                    let sid = SessionId::from(sid);
                    let tid = ThreadId::from(tid);
                    self.with_history(sid, tid, |history| {
                        history.append_message(message);
                    })
                    .await;
                }
                SessionEvent::TurnCompleted {
                    session_id: sid,
                    thread_id: tid,
                    usage,
                    context_item,
                    ..
                } => {
                    let sid = SessionId::from(sid);
                    let tid = ThreadId::from(tid);
                    let replay_key = (sid, tid.clone());
                    if !active_turns.contains(&replay_key) {
                        tracing::warn!(
                            "session replay: TurnCompleted without active turn (sid={sid}, tid={tid}) — skipped"
                        );
                        continue;
                    }
                    let context_item: TurnContextItem = serde_json::from_value(context_item)
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "session replay: failed to deserialize context_item: {e}"
                            );
                            TurnContext::new(sid, tid.clone()).to_turn_context_item()
                        });
                    let turn_context = TurnContext::from_item(context_item);
                    self.with_history(sid, tid, |history| {
                        history.finalize_turn(usage, &turn_context);
                    })
                    .await;
                    active_turns.remove(&replay_key);
                }
                SessionEvent::TurnDiscarded {
                    session_id: sid,
                    thread_id: tid,
                    ..
                } => {
                    let sid = SessionId::from(sid);
                    let tid = ThreadId::from(tid);
                    let replay_key = (sid, tid.clone());
                    if !active_turns.contains(&replay_key) {
                        tracing::warn!(
                            "session replay: TurnDiscarded without active turn (sid={sid}, tid={tid}) — skipped"
                        );
                        continue;
                    }
                    self.with_history(sid, tid, |history| {
                        history.discard_turn();
                    })
                    .await;
                    active_turns.remove(&replay_key);
                }
                SessionEvent::AgentRegistered { .. }
                | SessionEvent::AgentStatusChanged { .. }
                | SessionEvent::MailboxDelivered { .. } => {
                    // Collaboration events are persisted for supervisor replay, but the
                    // session transcript loader intentionally ignores them for now because
                    // they do not affect prompt-visible turn history.
                }
            }
        }

        match (session_id, thread_id) {
            (Some(sid), Some(tid)) => Ok((sid, tid)),
            _ => Err(crate::Error::Runtime {
                message: "no TurnStarted event found in session replay".to_string(),
                stage: "session-load-from-events".to_string(),
                inflight_snapshot: None,
            }),
        }
    }
}
