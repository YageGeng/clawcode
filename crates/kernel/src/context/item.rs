use llm::completion::Message;
use serde::{Deserialize, Serialize};

use crate::session::{SessionId, ThreadId};

/// Durable snapshot of the stable runtime fields for one completed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnContextItem {
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    #[serde(default)]
    pub subagent_depth: usize,
    pub name: Option<String>,
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub system_prompt: Option<String>,
    pub cwd: Option<String>,
    pub current_date: Option<String>,
    pub timezone: Option<String>,
}

impl TurnContextItem {
    /// Compares two durable snapshots and emits model-visible update messages for changed fields.
    pub fn diff_messages(&self, next: &TurnContextItem) -> Vec<Message> {
        let mut updates = Vec::new();

        if self.agent_id != next.agent_id {
            updates.push(Message::assistant(format!(
                "<context_update><field>agent_id</field><value>{:?}</value></context_update>",
                next.agent_id
            )));
        }
        if self.parent_agent_id != next.parent_agent_id {
            updates.push(Message::assistant(format!(
                "<context_update><field>parent_agent_id</field><value>{:?}</value></context_update>",
                next.parent_agent_id
            )));
        }
        if self.subagent_depth != next.subagent_depth {
            updates.push(Message::assistant(format!(
                "<context_update><field>subagent_depth</field><value>{:?}</value></context_update>",
                next.subagent_depth
            )));
        }
        if self.name != next.name {
            updates.push(Message::assistant(format!(
                "<context_update><field>name</field><value>{:?}</value></context_update>",
                next.name
            )));
        }

        if self.system_prompt != next.system_prompt {
            updates.push(Message::assistant(format!(
                "<context_update><field>system_prompt</field><value>{:?}</value></context_update>",
                next.system_prompt
            )));
        }
        if self.cwd != next.cwd {
            updates.push(Message::assistant(format!(
                "<context_update><field>cwd</field><value>{:?}</value></context_update>",
                next.cwd
            )));
        }
        if self.current_date != next.current_date {
            updates.push(Message::assistant(format!(
                "<context_update><field>current_date</field><value>{:?}</value></context_update>",
                next.current_date
            )));
        }
        if self.timezone != next.timezone {
            updates.push(Message::assistant(format!(
                "<context_update><field>timezone</field><value>{:?}</value></context_update>",
                next.timezone
            )));
        }

        updates
    }
}
