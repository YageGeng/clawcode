use protocol::{ContentBlock, EmbeddedResourceContent};

use crate::ModelError;

/// Converts one typed transcript block into an explicit text projection for providers.
pub(super) trait ProviderContentProjection {
    /// Consumes one block and returns text only when no native provider mapping exists.
    fn into_provider_text(self) -> Result<String, ModelError>;
}

impl ProviderContentProjection for ContentBlock {
    /// Preserves useful Resource and structured context without forwarding unsupported binary data.
    fn into_provider_text(self) -> Result<String, ModelError> {
        match self {
            Self::Text { text } | Self::Reasoning { text } => Ok(text),
            Self::Audio { mime_type, .. } => {
                Ok(format!("[Audio content: {mime_type}]"))
            }
            Self::EmbeddedResource {
                uri,
                mime_type,
                content,
            } => {
                let media = mime_type
                    .map_or_else(String::new, |value| format!(" ({value})"));
                match content {
                    EmbeddedResourceContent::Text { text } => Ok(format!(
                        "[Embedded resource: {uri}{media}]\n{text}"
                    )),
                    EmbeddedResourceContent::Blob { .. } => Ok(format!(
                        "[Embedded binary resource: {uri}{media}]"
                    )),
                }
            }
            Self::ResourceLink {
                uri,
                name,
                description,
                mime_type,
                ..
            } => {
                let media = mime_type
                    .map_or_else(String::new, |value| format!(" ({value})"));
                let description = description
                    .map_or_else(String::new, |value| format!("\n{value}"));
                Ok(format!(
                    "[Resource link: {name} — {uri}{media}]{description}"
                ))
            }
            Self::Structured { value } => {
                let value = serde_json::to_string_pretty(&value).map_err(
                    |error| {
                        ModelError::Protocol(format!(
                            "structured content serialization failed: {error}"
                        ))
                    },
                )?;
                Ok(format!("[Structured content]\n{value}"))
            }
            Self::Image { .. } | Self::ToolCall { .. } => {
                Err(ModelError::Protocol(
                    "native image and Tool Call blocks require role-specific provider mapping"
                        .to_string(),
                ))
            }
        }
    }
}
