//! Session identifier and metadata types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unique session identifier generated when a new session is created.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Summary info for a session in listing results.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    #[builder(default)]
    pub title: Option<String>,
    #[builder(default)]
    pub updated_at: Option<String>,
}

/// Data returned to the frontend after creating or loading a session.
#[derive(Debug, Clone)]
pub struct SessionCreated {
    pub session_id: SessionId,
    pub modes: Vec<super::config::SessionMode>,
    pub models: Vec<super::config::ModelInfo>,
}

/// Paginated session list result.
#[derive(Debug, Clone)]
pub struct SessionListPage {
    pub sessions: Vec<SessionInfo>,
    pub next_cursor: Option<String>,
}

impl From<SessionId> for String {
    fn from(session_id: SessionId) -> Self {
        session_id.0
    }
}

impl From<&SessionId> for String {
    fn from(session_id: &SessionId) -> Self {
        session_id.0.clone()
    }
}
