//! Multi-agent identity, status, and inter-agent messaging types.

use serde::{Deserialize, Serialize};

/// Hierarchical agent path, e.g. `/root/explorer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentPath(pub String);

impl std::fmt::Display for AgentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AgentPath {
    /// Create the root agent path.
    #[must_use]
    pub fn root() -> Self {
        Self("/root".to_string())
    }

    /// Create a child agent path under this parent.
    #[must_use]
    pub fn join(&self, name: &str) -> Self {
        Self(format!("{}/{}", self.0, name))
    }

    /// Extract the last segment (the agent's name).
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Return the inner path as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` if this is the root agent path (`/root`).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == "/root"
    }
}

/// Runtime status of an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent reserved but not yet started.
    PendingInit,
    Running,
    Interrupted,
    Completed {
        /// Optional final assistant message content.
        message: Option<String>,
    },
    Errored {
        /// Human-readable error description.
        reason: String,
    },
    Shutdown,
    /// Agent path or nickname not found in registry.
    NotFound,
}

/// Message sent between agents in a multi-agent session.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct InterAgentMessage {
    pub from: AgentPath,
    pub to: AgentPath,
    pub content: String,
    #[builder(default)]
    pub trigger_turn: bool,
}
