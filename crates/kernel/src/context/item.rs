use llm::completion::Message;

use crate::session::{SessionId, ThreadId};

/// Durable snapshot of the stable runtime fields for one completed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContextItem {
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

impl TurnContextItem {
    /// Compares two durable snapshots and emits model-visible update messages for changed fields.
    pub fn diff_messages(&self, next: &TurnContextItem) -> Vec<Message> {
        let mut updates = Vec::new();

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
