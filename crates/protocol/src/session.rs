//! Session identifier and metadata types.

use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema;
use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::usage::Usage;

/// Unique session identifier generated when a new session is created.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Arc<str>);

impl SessionId {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }
}

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
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct SessionCreated {
    /// Session id that was created or restored.
    pub session_id: SessionId,
    /// Current model id in `provider_id/model_id` form.
    pub current_model: String,
    /// Available session mode presets.
    pub modes: Vec<super::config::SessionMode>,
    /// Available model selections.
    pub models: Vec<super::config::ModelInfo>,
    /// Replayed message history when loading an existing session.
    #[builder(default)]
    pub history: Vec<Message>,
    /// Accumulated provider-reported usage for replayed history.
    #[builder(default)]
    pub history_usage: Option<Usage>,
}

/// Paginated session list result.
#[derive(Debug, Clone)]
pub struct SessionListPage {
    pub sessions: Vec<SessionInfo>,
    pub next_cursor: Option<String>,
}

impl From<SessionId> for String {
    fn from(session_id: SessionId) -> Self {
        session_id.0.to_string()
    }
}

impl From<&SessionId> for String {
    fn from(session_id: &SessionId) -> Self {
        session_id.0.to_string()
    }
}

impl From<String> for SessionId {
    fn from(id: String) -> Self {
        Self(id.into())
    }
}

impl From<&str> for SessionId {
    fn from(id: &str) -> Self {
        Self(id.into())
    }
}

impl From<Arc<str>> for SessionId {
    fn from(id: Arc<str>) -> Self {
        Self(id)
    }
}

impl From<schema::SessionId> for SessionId {
    fn from(session_id: schema::SessionId) -> Self {
        Self(session_id.0)
    }
}

impl From<SessionId> for schema::SessionId {
    fn from(session_id: SessionId) -> Self {
        schema::SessionId::new(session_id.0)
    }
}

impl From<&SessionId> for schema::SessionId {
    fn from(session_id: &SessionId) -> Self {
        schema::SessionId::new(Arc::clone(&session_id.0))
    }
}
