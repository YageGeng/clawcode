use std::sync::Arc;

use protocol::{
    AgentEvent, AgentEventPayload, EntryId, EventMetadata, ExtensionInvocation,
    IdKind, ModelProfile, ModelSelectEvent, ModelSelectSource, SessionId,
    SessionSettings, ThinkingLevel, ThinkingLevelSelectEvent, TurnId,
};
use store::{EntryKind, NewEntry, SessionEntry};

use super::{Kernel, KernelError, Session};

/// Borrows one live Session for persisted model or thinking changes.
///
/// Client requests and extension calls share this path, so both persist, notify
/// extension observers, and report `SettingsChanged` the same way.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct SessionSettingsUpdate<'a> {
    kernel: &'a Kernel,
    session_id: &'a SessionId,
    session: &'a Arc<Session>,
    /// Extension call that made the change; `None` for client requests.
    #[builder(default)]
    invocation: Option<&'a ExtensionInvocation>,
}

impl SessionSettingsUpdate<'_> {
    /// Persists and selects one configured model for future Turn snapshots.
    pub(in crate::runtime) async fn model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelProfile, KernelError> {
        let Some(model) = self.session.model.resolve(provider_id, model_id)
        else {
            tracing::warn!(
                "rejected model change for Session {} to unconfigured model {}/{}",
                self.session_id,
                provider_id,
                model_id
            );
            return Err(KernelError::Protocol(format!(
                "model is not configured: {provider_id}/{model_id}"
            )));
        };
        let previous_model = {
            let _serial = self.session.settings_changes.lock().await;
            let previous_model = self.session.model.active()?.profile().clone();
            if previous_model == *model.profile() {
                return Ok(previous_model);
            }
            self.persist(&SettingChange::Model {
                provider: provider_id.to_string(),
                model_id: model_id.to_string(),
            })?;
            self.session.model.replace_model(Arc::clone(&model))?;
            tracing::info!(
                "changed model for Session {} to {}/{}",
                self.session_id,
                provider_id,
                model_id
            );
            self.publish().await;
            previous_model
        };
        // Observers run unlocked, so one that changes settings again cannot deadlock.
        self.session
            .extensions
            .runtime_ref()
            .emit_model_select(
                &ModelSelectEvent {
                    model: model.profile().clone(),
                    previous_model: Some(previous_model),
                    source: ModelSelectSource::Set,
                },
                &self.extension_context()?,
            )
            .await;
        Ok(model.profile().clone())
    }

    /// Persists and selects one thinking level for future Turn snapshots.
    pub(in crate::runtime) async fn thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> Result<ThinkingLevel, KernelError> {
        let previous_level = {
            let _serial = self.session.settings_changes.lock().await;
            let previous_level = self.session.model.thinking_level()?;
            if previous_level == level {
                return Ok(level);
            }
            self.persist(&SettingChange::ThinkingLevel(level))?;
            self.session.model.replace_thinking_level(level)?;
            tracing::info!(
                "changed thinking level for Session {} to {:?}",
                self.session_id,
                level
            );
            self.publish().await;
            previous_level
        };
        // Observers run unlocked, so one that changes settings again cannot deadlock.
        self.session
            .extensions
            .runtime_ref()
            .emit_thinking_level_select(
                &ThinkingLevelSelectEvent {
                    level,
                    previous_level,
                },
                &self.extension_context()?,
            )
            .await;
        Ok(level)
    }

    /// Appends one durable setting entry before the in-memory selection changes.
    fn persist(&self, change: &SettingChange) -> Result<(), KernelError> {
        let (kind, payload) = change.into();
        let id =
            EntryId::try_from(self.kernel.id_generator.next(IdKind::Entry))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.session.transcript.append_store_only_entry(NewEntry {
            id,
            kind,
            payload,
        })?;
        self.session.sync_store()
    }

    /// Builds the extension context for the Run and Turn of the change, if any.
    fn extension_context(
        &self,
    ) -> Result<extension::ExtensionContext, KernelError> {
        self.kernel.extension_context(
            self.session,
            self.invocation
                .and_then(|invocation| invocation.run_id.as_ref()),
            self.invocation
                .and_then(|invocation| invocation.turn_id.as_ref()),
        )
    }

    /// Reports both current values; the change is already durable, so a sink failure only warns.
    async fn publish(&self) {
        let event = match self.settings_event() {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(
                    "failed to build settings event for Session {}: {}",
                    self.session_id,
                    error
                );
                return;
            }
        };
        if let Err(error) =
            self.kernel.session_sink(self.session_id).emit(event).await
        {
            tracing::warn!(
                "failed to report settings change for Session {}: {}",
                self.session_id,
                error
            );
        }
    }

    /// Snapshots the current selection into one `SettingsChanged` event.
    fn settings_event(&self) -> Result<AgentEvent, KernelError> {
        let turn_id = match self
            .invocation
            .and_then(|invocation| invocation.turn_id.clone())
        {
            Some(turn_id) => turn_id,
            None => {
                TurnId::try_from(self.kernel.id_generator.next(IdKind::Turn))
                    .map_err(|error| KernelError::Protocol(error.to_string()))?
            }
        };
        Ok(AgentEvent {
            metadata: EventMetadata {
                turn_id,
                timestamp_ms: self.kernel.clock.now(),
                sequence: self.session.execution.next_sequence()?,
            },
            payload: AgentEventPayload::SettingsChanged {
                settings: self.session.settings()?,
            },
        })
    }
}

impl Session {
    /// Captures the current selection and the catalog it was chosen from.
    pub(in crate::runtime) fn settings(
        &self,
    ) -> Result<SessionSettings, KernelError> {
        let (model, thinking_level) = self.model.snapshot()?;
        Ok(SessionSettings {
            model: model.profile().clone(),
            thinking_level,
            models: self.model.profiles(),
        })
    }
}

impl Kernel {
    /// Returns a Session's model, thinking level, and selectable models.
    pub fn session_settings(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionSettings, KernelError> {
        self.session(session_id)?.settings()
    }

    /// Selects a configured model; a running Turn keeps its snapshot and the next request uses it.
    pub async fn set_session_model(
        &self,
        session_id: &SessionId,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelProfile, KernelError> {
        let session = self.session(session_id)?;
        SessionSettingsUpdate::builder()
            .kernel(self)
            .session_id(session_id)
            .session(&session)
            .build()
            .model(provider_id, model_id)
            .await
    }

    /// Selects a thinking level; a running Turn keeps its snapshot and the next request uses it.
    pub async fn set_session_thinking_level(
        &self,
        session_id: &SessionId,
        level: ThinkingLevel,
    ) -> Result<ThinkingLevel, KernelError> {
        let session = self.session(session_id)?;
        SessionSettingsUpdate::builder()
            .kernel(self)
            .session_id(session_id)
            .session(&session)
            .build()
            .thinking_level(level)
            .await
    }
}

/// Owns the persisted payload shape for model and thinking selections.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::runtime) enum SettingChange {
    Model { provider: String, model_id: String },
    ThinkingLevel(ThinkingLevel),
}

impl From<&SettingChange> for (EntryKind, serde_json::Value) {
    /// Encodes settings using the existing Pi v4 entry payloads.
    fn from(change: &SettingChange) -> Self {
        match change {
            SettingChange::Model { provider, model_id } => (
                EntryKind::ModelChange,
                serde_json::json!({ "provider": provider, "modelId": model_id }),
            ),
            SettingChange::ThinkingLevel(level) => (
                EntryKind::ThinkingLevelChange,
                serde_json::json!({ "thinkingLevel": level }),
            ),
        }
    }
}

impl TryFrom<&SessionEntry> for SettingChange {
    type Error = KernelError;

    /// Rejects malformed settings instead of silently replacing saved choices.
    fn try_from(entry: &SessionEntry) -> Result<Self, Self::Error> {
        let result = match entry.kind {
            EntryKind::ModelChange => {
                let provider = entry
                    .payload
                    .get("provider")
                    .and_then(serde_json::Value::as_str);
                let model_id = entry
                    .payload
                    .get("modelId")
                    .and_then(serde_json::Value::as_str);
                match (provider, model_id) {
                    (Some(provider), Some(model_id)) => Ok(Self::Model {
                        provider: provider.into(),
                        model_id: model_id.into(),
                    }),
                    _ => Err(KernelError::Protocol(format!(
                        "model change {} has no provider or modelId",
                        entry.id
                    ))),
                }
            }
            EntryKind::ThinkingLevelChange => entry
                .payload
                .get("thinkingLevel")
                .ok_or_else(|| {
                    KernelError::Protocol(format!(
                        "thinking change {} has no thinkingLevel",
                        entry.id
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value(value.clone())
                        .map(Self::ThinkingLevel)
                        .map_err(KernelError::from)
                }),
            _ => Err(KernelError::Protocol(format!(
                "entry {} is not a model or thinking change",
                entry.id
            ))),
        };
        if let Err(error) = &result {
            tracing::warn!(
                "failed to restore setting entry {}: {}",
                entry.id,
                error
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use protocol::{EntryId, Sequence, ThinkingLevel, TimestampMs};
    use store::{EntryKind, SessionEntry};

    use super::SettingChange;

    /// Persisted settings retain their existing wire format and decode without loss.
    #[test]
    fn setting_changes_round_trip() {
        for (change, expected_kind, expected_payload) in [
            (
                SettingChange::Model {
                    provider: "fixture".into(),
                    model_id: "secondary".into(),
                },
                EntryKind::ModelChange,
                serde_json::json!({"provider":"fixture", "modelId":"secondary"}),
            ),
            (
                SettingChange::ThinkingLevel(ThinkingLevel::High),
                EntryKind::ThinkingLevelChange,
                serde_json::json!({"thinkingLevel":"high"}),
            ),
        ] {
            let (kind, payload) =
                <(EntryKind, serde_json::Value)>::from(&change);
            assert_eq!(kind, expected_kind);
            assert_eq!(payload, expected_payload);
            let entry = SessionEntry::builder()
                .id(EntryId::try_from("setting").expect("id"))
                .sequence(Sequence::try_from(1).expect("sequence"))
                .timestamp_ms(TimestampMs::from(1_u64))
                .kind(kind)
                .payload(payload.as_object().expect("object").clone())
                .build();
            assert_eq!(
                SettingChange::try_from(&entry).expect("decode"),
                change
            );
        }
    }

    /// Corrupt settings and unrelated entries cannot silently become active settings.
    #[test]
    fn invalid_setting_entries_are_rejected() {
        for kind in [
            EntryKind::Custom,
            EntryKind::ModelChange,
            EntryKind::ThinkingLevelChange,
        ] {
            let entry = SessionEntry::builder()
                .id(EntryId::try_from("invalid").expect("id"))
                .sequence(Sequence::try_from(1).expect("sequence"))
                .timestamp_ms(TimestampMs::from(1_u64))
                .kind(kind)
                .payload(serde_json::Map::new())
                .build();
            assert!(matches!(
                SettingChange::try_from(&entry),
                Err(super::KernelError::Protocol(_))
            ));
        }
    }
}
