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

#[derive(Debug, Clone, Default)]
struct ThreadState {
    turns: Vec<Turn>,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
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

        if messages.len() > limit {
            messages = messages.split_off(messages.len() - limit);
        }

        Ok(messages)
    }
}
