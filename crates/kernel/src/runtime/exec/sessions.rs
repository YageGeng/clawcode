//! `<command_sessions>` notes: the model learns which command sessions remain.
//!
//! A note is appended only when the table differs from the last note in
//! context, is stored as a clawcode custom entry, and is rebuilt from that
//! entry on replay, exactly like plan-mode notes; so the prompt prefix stays
//! stable and live and rebuilt contexts match.

use ::tools::exec::CommandSessions;
use protocol::TurnId;
use serde::{Deserialize, Serialize};

use crate::runtime::session::{ClawcodeEntry, StatedNote};
use crate::runtime::{Kernel, KernelError, Session};

/// One stated command-sessions snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::runtime) struct CommandSessionsNote {
    pub(in crate::runtime) sessions: CommandSessions,
    /// Turn the note belongs to, so the rebuilt note keeps its live identity.
    pub(in crate::runtime) turn_id: TurnId,
}

impl ClawcodeEntry for CommandSessionsNote {
    /// Also the type of the extension message the note becomes.
    const CUSTOM_TYPE: &'static str = "command_sessions";
}

impl StatedNote for CommandSessionsNote {
    type State = CommandSessions;
    const ID_PREFIX: &'static str = "command-sessions";

    /// Wraps the session table stated before `turn_id`.
    fn new(sessions: CommandSessions, turn_id: TurnId) -> Self {
        Self { sessions, turn_id }
    }

    /// Returns the stated session table.
    fn state(&self) -> &CommandSessions {
        &self.sessions
    }

    /// Returns the Turn the note belongs to.
    fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Renders the `<command_sessions>` table.
    fn render(&self) -> String {
        self.sessions.render()
    }
}

impl Session {
    /// Appends a note before a model request when the command sessions differ
    /// from what the context last stated; no note at all means none.
    pub(in crate::runtime) async fn refresh_command_sessions(
        &self,
        kernel: &Kernel,
        turn_id: &TurnId,
    ) -> Result<(), KernelError> {
        let Some(exec) = &self.exec else {
            return Ok(());
        };
        if !exec.settings().config.command_sessions {
            return Ok(());
        }
        let current = exec.sessions().await;
        self.refresh_note::<CommandSessionsNote>(kernel, turn_id, current)
    }
}
