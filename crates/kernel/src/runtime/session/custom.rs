//! Values the Kernel persists as its own custom Store entries.

use protocol::{
    AgentMessage, ContentBlock, EntryId, ExtensionId, ExtensionMessage, IdKind,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ProductIdentity,
    TimestampMs, TurnId,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use store::{EntryKind, SessionEntry};

use crate::runtime::{Kernel, KernelError, Session};

/// A value stored as a clawcode custom entry `{extensionId, customType, data}`,
/// where `data` is the value's own serde form.
pub(in crate::runtime) trait ClawcodeEntry:
    Serialize + DeserializeOwned
{
    /// `customType` that tells this value apart from other clawcode entries.
    const CUSTOM_TYPE: &'static str;

    /// Encodes the value in the extension-entry shape clawcode already writes.
    fn payload(&self) -> Result<serde_json::Value, serde_json::Error> {
        Ok(serde_json::json!({
            "extensionId": ProductIdentity::SLUG,
            "customType": Self::CUSTOM_TYPE,
            "data": serde_json::to_value(self)?,
        }))
    }

    /// Reads the value an entry records. Other entries are not this value; a
    /// clawcode entry this version cannot read, such as one a later version
    /// wrote, is skipped with a warning instead of making the Session
    /// unopenable.
    fn from_entry(entry: &SessionEntry) -> Option<Self> {
        let field = |name: &str| {
            entry.payload.get(name).and_then(serde_json::Value::as_str)
        };
        if entry.kind != EntryKind::Custom
            || field("extensionId") != Some(ProductIdentity::SLUG)
            || field("customType") != Some(Self::CUSTOM_TYPE)
        {
            return None;
        }
        let data = entry.payload.get("data").cloned().unwrap_or_default();
        serde_json::from_value(data)
            .map_err(|error| {
                tracing::warn!(
                    "skipped unreadable {} entry {}: {}",
                    Self::CUSTOM_TYPE,
                    entry.id,
                    error
                );
            })
            .ok()
    }
}

/// A hidden context note that states one snapshot of Session state.
///
/// A note is appended only when the snapshot differs from the last one in
/// context, is stored as a clawcode custom entry, and is rebuilt from that
/// entry on replay, so the prompt prefix stays stable and live and rebuilt
/// contexts match.
pub(in crate::runtime) trait StatedNote:
    ClawcodeEntry
{
    /// The stated snapshot; a context without any note states the default.
    type State: PartialEq + Default + Serialize + DeserializeOwned;
    /// Prefix of the note message identifier, followed by the entry id.
    const ID_PREFIX: &'static str;

    /// Wraps a snapshot stated before `turn_id`.
    fn new(state: Self::State, turn_id: TurnId) -> Self;

    /// Returns the stated snapshot.
    fn state(&self) -> &Self::State;

    /// Turn the note belongs to, so the rebuilt note keeps its live identity.
    fn turn_id(&self) -> &TurnId;

    /// Renders the snapshot as the model reads it.
    fn render(&self) -> String;

    /// Builds the hidden context message; its identity comes from the stored entry.
    fn message(
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
                    format!("{}-{entry_id}", Self::ID_PREFIX).as_str(),
                )
                .map_err(protocol_error)?,
                turn_id: self.turn_id().clone(),
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
                        text: self.render(),
                    }])
                    .display(false)
                    .include_in_context(true)
                    .details(serde_json::to_value(self.state())?)
                    .build(),
            },
        })
    }

    /// Rebuilds the context message of a stored note entry, if it is one.
    fn rebuild(
        entry: &SessionEntry,
    ) -> Result<Option<AgentMessage>, KernelError> {
        Self::from_entry(entry)
            .map(|note| note.message(&entry.id, entry.timestamp_ms))
            .transpose()
    }

    /// Returns the snapshot a context message states, if it is one of these notes.
    fn stated(message: &AgentMessage) -> Option<Self::State> {
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
    /// Appends a note before a model request when `current` differs from the
    /// snapshot the context last stated; no note at all states the default.
    pub(in crate::runtime) fn refresh_note<N: StatedNote>(
        &self,
        kernel: &Kernel,
        turn_id: &TurnId,
        current: N::State,
    ) -> Result<(), KernelError> {
        let stated = self.transcript.inspect_history(|history| {
            history.iter().rev().find_map(N::stated).unwrap_or_default()
        })?;
        if stated == current {
            return Ok(());
        }
        let entry_id =
            EntryId::try_from(kernel.id_generator.next(IdKind::Entry))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        tracing::info!("stated {} before turn {}", N::CUSTOM_TYPE, turn_id);
        self.transcript
            .append_note(entry_id, &N::new(current, turn_id.clone()))?;
        self.sync_store()
    }
}
