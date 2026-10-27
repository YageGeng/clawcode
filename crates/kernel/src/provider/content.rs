use protocol::ContentBlock;

use crate::ModelError;

/// Converts transcript content with the same text view used by the output budget.
pub(super) trait ProviderContentProjection {
    /// Consumes one non-native block and projects its model-visible text.
    fn into_provider_text(self) -> Result<String, ModelError>;
}

impl ProviderContentProjection for ContentBlock {
    /// Reuses bounded-output formatting so JSON expansion cannot bypass accounting.
    fn into_provider_text(self) -> Result<String, ModelError> {
        if let Self::Text { text } | Self::Reasoning { text } = self {
            return Ok(text);
        }
        let view = tools::ToolContentText::try_from(&self)
            .map_err(|error| ModelError::Protocol(error.to_string()))?;
        let mut bytes = Vec::new();
        view.write_to(&mut bytes)
            .map_err(|error| ModelError::Protocol(error.to_string()))?;
        String::from_utf8(bytes)
            .map_err(|error| ModelError::Protocol(error.to_string()))
    }
}
