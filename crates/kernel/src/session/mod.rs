use std::fmt;

use llm::{completion::Message, usage::Usage};
use uuid::Uuid;

use crate::context::SessionTaskContext;

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

/// Describes one queued continuation request that the outer task loop may consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionContinuationRequest {
    PendingInput { input: String },
    SystemFollowUp { input: String },
}

/// Default in-memory session store now backed by the new session task context.
pub type InMemorySessionStore = SessionTaskContext;
