//! Immutable process logging configuration.

use serde::{Deserialize, Serialize};

const DEFAULT_LOG_FILTER: &str = "info";

/// Tracing filter directives accepted from the application configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingConfig {
    /// Complete `EnvFilter` directive used when `RUST_LOG` is absent.
    #[serde(default = "default_log_filter")]
    pub filter: String,
    /// Whether formatted log events include ANSI color escape sequences.
    #[serde(default)]
    pub color: bool,
}

impl Default for LoggingConfig {
    /// Uses informational logging without enabling verbose dependency output.
    fn default() -> Self {
        Self {
            filter: default_log_filter(),
            color: false,
        }
    }
}

/// Returns the stable tracing filter used when no explicit value is configured.
fn default_log_filter() -> String {
    DEFAULT_LOG_FILTER.to_string()
}
