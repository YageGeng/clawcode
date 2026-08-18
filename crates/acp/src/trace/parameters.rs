//! Lazy ACP parameter serialization and credential redaction.

use std::io::Read as _;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use serde_json::Value;

use crate::input::{MAX_ENCODED_IMAGE_BYTES, MAX_IMAGE_BYTES};

const ACP_TRACE_TARGET: &str = "acp::trace";
const REDACTED_VALUE: &str = "[REDACTED]";

/// Builds an exact image byte summary with bounded streaming decode memory.
fn summarize_image_data(data: &str) -> String {
    if data.len() > MAX_ENCODED_IMAGE_BYTES {
        return format!(
            "image data omitted: exceeds {} bytes",
            MAX_IMAGE_BYTES
        );
    }

    let mut decoder =
        base64::read::DecoderReader::new(data.as_bytes(), &BASE64_STANDARD);
    let mut buffer = [0_u8; 8 * 1024];
    let mut decoded_bytes = 0_usize;
    loop {
        match decoder.read(&mut buffer) {
            Ok(0) => {
                return format!("image data omitted: {} bytes", decoded_bytes);
            }
            Ok(read) => decoded_bytes = decoded_bytes.saturating_add(read),
            Err(_) => {
                return "image data omitted: invalid base64".to_string();
            }
        }
    }
}

/// Parameters retained only when ACP debug diagnostics are enabled.
#[derive(Clone, Default)]
pub(super) enum AcpLogParameters {
    /// Parameter serialization is unnecessary at the active log level.
    #[default]
    Disabled,
    /// Complete single-line JSON after recursive credential redaction.
    Json(Arc<str>),
    /// Serialization failed without affecting request handling.
    Unavailable(Arc<str>),
}

impl AcpLogParameters {
    /// Serializes and redacts parameters only when their debug event is enabled.
    pub(super) fn capture<T>(parameters: &T) -> Self
    where
        T: Serialize + ?Sized,
    {
        if !tracing::enabled!(target: ACP_TRACE_TARGET, tracing::Level::DEBUG) {
            return Self::Disabled;
        }

        match serde_json::to_value(parameters) {
            Ok(mut value) => {
                value.redact_sensitive();
                Self::Json(value.to_string().into())
            }
            Err(error) => Self::Unavailable(error.to_string().into()),
        }
    }

    /// Writes one readable parameter event without changing operation results.
    pub(super) fn log(&self, description: &str) {
        match self {
            Self::Disabled => {}
            Self::Json(parameters) => tracing::debug!(
                target: ACP_TRACE_TARGET,
                "parameters for {}: {}",
                description,
                parameters
            ),
            Self::Unavailable(error) => tracing::debug!(
                target: ACP_TRACE_TARGET,
                "parameters for {} are unavailable: {}",
                description,
                error
            ),
        }
    }
}

/// Recursively removes credential values while preserving complete JSON shape.
trait RedactSensitive {
    /// Replaces sensitive object values and descends through safe containers.
    fn redact_sensitive(&mut self);
}

impl RedactSensitive for Value {
    fn redact_sensitive(&mut self) {
        match self {
            Self::Object(fields) => {
                let image_data = fields
                    .get("type")
                    .and_then(Self::as_str)
                    .is_some_and(|kind| kind == "image");
                for (name, value) in fields {
                    if image_data && name == "data" {
                        let summary = value
                            .as_str()
                            .map(summarize_image_data)
                            .unwrap_or_else(|| {
                                "image data omitted: invalid base64".to_string()
                            });
                        *value = Self::String(summary);
                    } else if SensitiveParameterName::from(name.as_str())
                        .is_sensitive()
                    {
                        *value = Self::String(REDACTED_VALUE.to_string());
                    } else {
                        value.redact_sensitive();
                    }
                }
            }
            Self::Array(values) => {
                for value in values {
                    value.redact_sensitive();
                }
            }
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => {}
        }
    }
}

/// Normalized JSON field name used by the credential policy.
struct SensitiveParameterName(String);

impl From<&str> for SensitiveParameterName {
    /// Removes separators and case distinctions from one JSON field name.
    fn from(name: &str) -> Self {
        Self(
            name.chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .map(|character| character.to_ascii_lowercase())
                .collect(),
        )
    }
}

impl SensitiveParameterName {
    /// Identifies credential fields without hiding plural token counters.
    fn is_sensitive(&self) -> bool {
        !self.0.ends_with("tokens")
            && [
                "apikey",
                "token",
                "authorization",
                "cookie",
                "password",
                "secret",
            ]
            .iter()
            .any(|suffix| self.0.ends_with(suffix))
    }
}

#[cfg(test)]
mod tests {
    use super::RedactSensitive;

    /// Oversized image logging replaces data without decoding the complete payload.
    #[test]
    fn oversized_image_data_is_summarized_by_limit() {
        let mut value = serde_json::json!({
            "type": "image",
            "data": "A".repeat(13_981_020),
        });

        value.redact_sensitive();

        assert_eq!(value["data"], "image data omitted: exceeds 10485760 bytes");
    }
}
