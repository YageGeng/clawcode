//! Values the Kernel persists as its own custom Store entries.

use protocol::ProductIdentity;
use serde::Serialize;
use serde::de::DeserializeOwned;
use store::{EntryKind, SessionEntry};

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
