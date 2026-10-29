use protocol::ThinkingLevel;
use store::{EntryKind, SessionEntry};

use super::KernelError;

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
