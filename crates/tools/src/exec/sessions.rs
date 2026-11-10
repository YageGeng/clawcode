//! The command sessions the model is told about, as codex renders them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Sessions listed before the rest are only counted.
const MAX_LISTED: usize = 16;

/// What the model may do with one session.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct CommandSessionState {
    /// The process exited and its result waits for `write_stdin`.
    pub exited: bool,
    /// A call is currently writing to or polling the session.
    pub in_use: bool,
}

/// A bounded snapshot of a Session's command sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSessions {
    /// Listed sessions by session ID.
    pub sessions: BTreeMap<i32, CommandSessionState>,
    /// Sessions beyond the listing limit.
    pub omitted: usize,
}

impl FromIterator<(i32, CommandSessionState)> for CommandSessions {
    /// Keeps the lowest session IDs and counts the rest, as codex does.
    fn from_iter<I: IntoIterator<Item = (i32, CommandSessionState)>>(
        iter: I,
    ) -> Self {
        let mut snapshot = Self::default();
        for (id, state) in iter {
            snapshot.sessions.insert(id, state);
            if snapshot.sessions.len() > MAX_LISTED {
                snapshot.sessions.pop_last();
                snapshot.omitted += 1;
            }
        }
        snapshot
    }
}

impl CommandSessions {
    /// Renders the developer note the model reads.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::from(
            "Current command sessions for this thread (supersedes earlier snapshots). Exited sessions retain results for write_stdin. In-use sessions are temporarily busy with a tool interaction or result finalization.\n",
        );
        if self.sessions.is_empty() && self.omitted == 0 {
            text.push_str("None.");
        }
        for (id, state) in &self.sessions {
            let status = if state.exited {
                "exited; poll to collect"
            } else {
                "running"
            };
            let in_use = if state.in_use { "; in use" } else { "" };
            text.push_str(&format!("{id}: {status}{in_use}\n"));
        }
        if self.omitted > 0 {
            text.push_str(&format!(
                "{} additional sessions omitted; this snapshot is incomplete.",
                self.omitted
            ));
        }
        format!(
            "<command_sessions>\n{}\n</command_sessions>",
            text.trim_end()
        )
    }
}
