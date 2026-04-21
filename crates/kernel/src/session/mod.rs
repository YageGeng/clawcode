use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use llm::{completion::Message, usage::Usage};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::Result;

/// Stable session identifier used by the runtime and adapters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionId {
    /// Creates a fresh session identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stable thread identifier used to separate conversations inside a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadId(Uuid);

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadId {
    /// Creates a fresh thread identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One persisted turn containing the input transcript and token usage.
#[derive(Debug, Clone)]
pub struct Turn {
    pub user_text: String,
    pub transcript: Vec<Message>,
    pub usage: Usage,
}

impl Turn {
    /// Builds a turn that can be written to a session store after a run completes.
    pub fn new(user_text: impl Into<String>, transcript: Vec<Message>, usage: Usage) -> Self {
        Self {
            user_text: user_text.into(),
            transcript,
            usage,
        }
    }
}

/// Mutable turn state kept in memory until the runtime finalizes the turn.
#[derive(Debug, Clone)]
struct ActiveTurn {
    user_text: String,
    transcript: Vec<Message>,
}

/// Describes one queued continuation request that the outer task loop may consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionContinuationRequest {
    PendingInput { input: String },
    SystemFollowUp { input: String },
}

#[derive(Debug, Clone, Default)]
struct ThreadState {
    turns: Vec<Turn>,
    active_turn: Option<ActiveTurn>,
    continuations: Vec<SessionContinuationRequest>,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Starts a new in-progress turn so incremental messages become visible immediately.
    async fn begin_turn(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        user_text: String,
        user_message: Message,
    ) -> Result<()>;

    /// Appends one message to the active turn transcript before the turn finalizes.
    async fn append_message(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        message: Message,
    ) -> Result<()>;

    /// Finalizes the active turn and moves it into completed thread history.
    async fn finalize_turn(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        usage: Usage,
    ) -> Result<()>;

    /// Discards the active turn when execution fails before completion.
    async fn discard_turn(&self, session_id: SessionId, thread_id: ThreadId) -> Result<()>;

    /// Queues one structured continuation request for the addressed thread.
    async fn enqueue_continuation(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        continuation: SessionContinuationRequest,
    ) -> Result<()>;

    /// Drains the oldest queued continuation request for the addressed thread, if any.
    async fn take_continuation(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Result<Option<SessionContinuationRequest>>;

    /// Queues a follow-up user input to be submitted by the outer task loop after the active turn.
    async fn enqueue_pending_input(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        input: String,
    ) -> Result<()> {
        self.enqueue_continuation(
            session_id,
            thread_id,
            SessionContinuationRequest::PendingInput { input },
        )
        .await
    }

    /// Atomically drains the oldest queued follow-up user input for the addressed thread, if any.
    async fn take_pending_input(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Result<Option<String>>;

    /// Appends a completed turn to the addressed session thread.
    async fn append_turn(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        turn: Turn,
    ) -> Result<()>;

    /// Loads the most recent transcript messages for one session thread.
    async fn load_messages(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<Message>>;
}

/// In-memory session store for the first milestone and tests.
#[derive(Default)]
pub struct InMemorySessionStore {
    threads: RwLock<HashMap<(SessionId, ThreadId), ThreadState>>,
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    /// Starts an in-progress turn and seeds it with the user's submitted message.
    async fn begin_turn(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        user_text: String,
        user_message: Message,
    ) -> Result<()> {
        let mut threads = self.threads.write().await;
        let thread = threads.entry((session_id, thread_id)).or_default();
        if thread.active_turn.is_some() {
            return Err(crate::Error::Runtime {
                message: "cannot begin a new turn while another turn is active".to_string(),
                stage: "session-begin-turn".to_string(),
                inflight_snapshot: None,
            });
        }

        thread.active_turn = Some(ActiveTurn {
            user_text,
            transcript: vec![user_message],
        });
        Ok(())
    }

    /// Appends one message to the current active turn transcript.
    async fn append_message(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        message: Message,
    ) -> Result<()> {
        let mut threads = self.threads.write().await;
        let thread =
            threads
                .get_mut(&(session_id, thread_id))
                .ok_or_else(|| crate::Error::Runtime {
                    message: "cannot append a message without an existing thread".to_string(),
                    stage: "session-append-message".to_string(),
                    inflight_snapshot: None,
                })?;
        let active_turn = thread
            .active_turn
            .as_mut()
            .ok_or_else(|| crate::Error::Runtime {
                message: "cannot append a message without an active turn".to_string(),
                stage: "session-append-message".to_string(),
                inflight_snapshot: None,
            })?;

        active_turn.transcript.push(message);
        Ok(())
    }

    /// Moves the active turn into completed history with the supplied usage totals.
    async fn finalize_turn(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        usage: Usage,
    ) -> Result<()> {
        let mut threads = self.threads.write().await;
        let thread =
            threads
                .get_mut(&(session_id, thread_id))
                .ok_or_else(|| crate::Error::Runtime {
                    message: "cannot finalize a turn without an existing thread".to_string(),
                    stage: "session-finalize-turn".to_string(),
                    inflight_snapshot: None,
                })?;
        let active_turn = thread
            .active_turn
            .take()
            .ok_or_else(|| crate::Error::Runtime {
                message: "cannot finalize a turn without an active turn".to_string(),
                stage: "session-finalize-turn".to_string(),
                inflight_snapshot: None,
            })?;

        thread.turns.push(Turn::new(
            active_turn.user_text,
            active_turn.transcript,
            usage,
        ));
        Ok(())
    }

    /// Drops the active turn when execution aborts before a valid final result exists.
    async fn discard_turn(&self, session_id: SessionId, thread_id: ThreadId) -> Result<()> {
        let mut threads = self.threads.write().await;
        if let Some(thread) = threads.get_mut(&(session_id, thread_id)) {
            thread.active_turn = None;
        }
        Ok(())
    }

    /// Queues a follow-up input so the outer task loop can continue with another turn.
    async fn enqueue_continuation(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        continuation: SessionContinuationRequest,
    ) -> Result<()> {
        let mut threads = self.threads.write().await;
        let thread = threads.entry((session_id, thread_id)).or_default();
        thread.continuations.push(continuation);
        Ok(())
    }

    /// Pops the oldest queued continuation request for the addressed thread.
    async fn take_continuation(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
    ) -> Result<Option<SessionContinuationRequest>> {
        let mut threads = self.threads.write().await;
        let Some(thread) = threads.get_mut(&(session_id, thread_id)) else {
            return Ok(None);
        };
        if thread.continuations.is_empty() {
            return Ok(None);
        }
        Ok(Some(thread.continuations.remove(0)))
    }

    /// Atomically drains the oldest pending-input continuation while preserving other queue entries.
    async fn take_pending_input(
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
                    unreachable!("the matched continuation should remain a pending input");
                };
                Ok(Some(input))
            }
            SessionContinuationRequest::SystemFollowUp { .. } => Err(crate::Error::Runtime {
                message: "take_pending_input cannot consume system follow-up continuation requests"
                    .to_string(),
                stage: "session-take-pending-input".to_string(),
                inflight_snapshot: None,
            }),
        }
    }

    /// Appends one completed turn to the in-memory thread state.
    async fn append_turn(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        turn: Turn,
    ) -> Result<()> {
        let mut threads = self.threads.write().await;
        let thread = threads.entry((session_id, thread_id)).or_default();
        thread.turns.push(turn);
        Ok(())
    }

    /// Returns up to `limit` most recent transcript messages for the thread.
    async fn load_messages(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<Message>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let threads = self.threads.read().await;
        let Some(thread) = threads.get(&(session_id, thread_id)) else {
            return Ok(Vec::new());
        };

        let mut messages = thread
            .turns
            .iter()
            .flat_map(|turn| turn.transcript.clone())
            .collect::<Vec<_>>();
        if let Some(active_turn) = thread.active_turn.as_ref() {
            messages.extend(active_turn.transcript.clone());
        }

        if messages.len() > limit {
            messages = messages.split_off(messages.len() - limit);
        }

        Ok(messages)
    }
}
