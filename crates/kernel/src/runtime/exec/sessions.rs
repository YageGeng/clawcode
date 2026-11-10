//! `<command_sessions>` notes: the model learns which command sessions remain.
//!
//! A note is appended only when the table differs from the last note in
//! context, is stored as a clawcode custom entry, and is rebuilt from that
//! entry on replay, exactly like plan-mode notes; so the prompt prefix stays
//! stable and live and rebuilt contexts match.

use ::tools::exec::CommandSessions;
use protocol::{
    AgentMessage, ContentBlock, EntryId, ExtensionId, ExtensionMessage, IdKind,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ProductIdentity,
    TimestampMs, TurnId,
};
use serde::{Deserialize, Serialize};

use crate::runtime::session::ClawcodeEntry;
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

impl CommandSessionsNote {
    /// Builds the hidden context message; its identity comes from the stored entry.
    pub(in crate::runtime) fn message(
        &self,
        entry_id: &EntryId,
        at: TimestampMs,
    ) -> Result<AgentMessage, KernelError> {
        let protocol_error = |error: protocol::ScalarError| {
            KernelError::Protocol(error.to_string())
        };
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(
                    format!("command-sessions-{entry_id}").as_str(),
                )
                .map_err(protocol_error)?,
                turn_id: self.turn_id.clone(),
            },
            timing: MessageTiming::try_from((at, at, at))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::Extension {
                extension: ExtensionMessage::builder()
                    .extension_id(
                        ExtensionId::try_from(ProductIdentity::SLUG)
                            .map_err(protocol_error)?,
                    )
                    .custom_type(Self::CUSTOM_TYPE.to_string())
                    .blocks(vec![ContentBlock::Text {
                        text: self.sessions.render(),
                    }])
                    .display(false)
                    .include_in_context(true)
                    .details(serde_json::to_value(&self.sessions)?)
                    .build(),
            },
        })
    }

    /// Returns the snapshot a context message states, if it is one of these notes.
    pub(in crate::runtime) fn stated(
        message: &AgentMessage,
    ) -> Option<CommandSessions> {
        match &message.content {
            MessageContent::Extension { extension }
                if extension.custom_type == Self::CUSTOM_TYPE
                    && extension.extension_id.as_str()
                        == ProductIdentity::SLUG =>
            {
                extension
                    .details
                    .clone()
                    .and_then(|details| serde_json::from_value(details).ok())
            }
            _ => None,
        }
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
        let stated = self.transcript.inspect_history(|history| {
            history
                .iter()
                .rev()
                .find_map(CommandSessionsNote::stated)
                .unwrap_or_default()
        })?;
        if stated == current {
            return Ok(());
        }
        let entry_id =
            EntryId::try_from(kernel.id_generator.next(IdKind::Entry))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        tracing::info!(
            "stated {} command sessions ({} omitted) before turn {}",
            current.sessions.len(),
            current.omitted,
            turn_id
        );
        self.transcript.append_command_sessions(
            entry_id,
            &CommandSessionsNote {
                sessions: current,
                turn_id: turn_id.clone(),
            },
        )?;
        self.sync_store()
    }
}
