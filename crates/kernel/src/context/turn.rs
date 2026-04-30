use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    context::TurnContextItem,
    session::{SessionId, ThreadId},
};

/// Complete runtime context carried by one turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnContext {
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    pub name: Option<String>,
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub system_prompt: Option<String>,
    pub cwd: Option<String>,
    pub current_date: Option<String>,
    pub timezone: Option<String>,
}

impl TurnContext {
    /// Builds a root turn context bound to one session/thread pair.
    pub fn new(session_id: SessionId, thread_id: ThreadId) -> Self {
        Self {
            agent_id: Uuid::new_v4().to_string(),
            parent_agent_id: None,
            name: None,
            session_id,
            thread_id,
            system_prompt: None,
            cwd: None,
            current_date: None,
            timezone: None,
        }
    }

    /// Attaches a human-readable name to the turn context.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Records the parent agent identifier for a derived child context.
    pub fn with_parent_agent_id(mut self, parent_agent_id: impl Into<String>) -> Self {
        self.parent_agent_id = Some(parent_agent_id.into());
        self
    }

    /// Attaches a system prompt that should be stable for the turn.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    /// Attaches a stable working-directory string for downstream runtime consumers.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Attaches the effective current date exposed to the agent.
    pub fn with_current_date(mut self, current_date: impl Into<String>) -> Self {
        self.current_date = Some(current_date.into());
        self
    }

    /// Attaches the effective timezone exposed to the agent.
    pub fn with_timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = Some(timezone.into());
        self
    }

    /// Reconstructs a full runtime context from a durable snapshot (e.g. after session replay).
    pub fn from_item(item: TurnContextItem) -> Self {
        Self {
            agent_id: item.agent_id,
            parent_agent_id: item.parent_agent_id,
            name: item.name,
            session_id: item.session_id,
            thread_id: item.thread_id,
            system_prompt: item.system_prompt,
            cwd: item.cwd,
            current_date: item.current_date,
            timezone: item.timezone,
        }
    }

    /// Converts the runtime context into a durable baseline snapshot.
    pub fn to_turn_context_item(&self) -> TurnContextItem {
        TurnContextItem {
            agent_id: self.agent_id.clone(),
            parent_agent_id: self.parent_agent_id.clone(),
            name: self.name.clone(),
            session_id: self.session_id,
            thread_id: self.thread_id.clone(),
            system_prompt: self.system_prompt.clone(),
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
        }
    }

    /// Forks a child context that inherits the stable parent scope and prompt state.
    pub fn fork_child(&self, name: impl Into<String>) -> Self {
        Self {
            agent_id: Uuid::new_v4().to_string(),
            parent_agent_id: Some(self.agent_id.clone()),
            name: Some(name.into()),
            session_id: self.session_id,
            thread_id: self.thread_id.clone(),
            system_prompt: self.system_prompt.clone(),
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
        }
    }

    /// Forks a child context while preserving the legacy `AgentContext` API shape.
    pub fn fork(&self, name: impl Into<String>) -> Self {
        self.fork_child(name)
    }
}
