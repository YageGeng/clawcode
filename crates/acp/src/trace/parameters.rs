//! Lazy ACP parameter serialization and credential redaction.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

const ACP_TRACE_TARGET: &str = "acp::trace";
const REDACTED_VALUE: &str = "[REDACTED]";

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
                for (name, value) in fields {
                    if SensitiveParameterName::from(name.as_str())
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
