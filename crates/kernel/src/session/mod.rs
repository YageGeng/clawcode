use std::{fmt, str::FromStr};

use llm::{completion::Message, usage::Usage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::context::SessionTaskContext;

/// Stable session identifier used by the runtime and adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Returns the inner UUID value.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for SessionId {
    /// Wraps a UUID in the stable runtime session identifier type.
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl FromStr for SessionId {
    type Err = uuid::Error;

    /// Parses a user-facing UUID string into a session identifier.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self::from)
    }
}

impl TryFrom<&str> for SessionId {
    type Error = uuid::Error;

    /// Parses a borrowed UUID string into a session identifier.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<SessionId> for String {
    /// Converts a typed session identifier into its stable string representation.
    fn from(value: SessionId) -> Self {
        value.to_string()
    }
}

/// Stable thread identifier used to separate conversations inside a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Returns the inner UUID value.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for ThreadId {
    /// Wraps a UUID in the stable runtime thread identifier type.
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl FromStr for ThreadId {
    type Err = uuid::Error;

    /// Parses a user-facing UUID string into a thread identifier.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self::from)
    }
}

impl TryFrom<&str> for ThreadId {
    type Error = uuid::Error;

    /// Parses a borrowed UUID string into a thread identifier.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<ThreadId> for String {
    /// Converts a typed thread identifier into its stable string representation.
    fn from(value: ThreadId) -> Self {
        value.to_string()
    }
}

/// One persisted turn containing the input transcript and token usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionContinuationRequest {
    PendingInput { input: String },
    SystemFollowUp { input: String },
}

/// Default in-memory session store now backed by the new session task context.
pub type InMemorySessionStore = SessionTaskContext;
