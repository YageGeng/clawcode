use std::io;

use snafu::Snafu;

/// Shared tool error type for the extracted tools crate.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("json error on `{stage}`, {source}"))]
    Json {
        source: serde_json::Error,
        stage: String,
    },

    #[snafu(display("tool `{tool}` timed out on `{stage}`, {source}"))]
    ToolTimeout {
        source: tokio::time::error::Elapsed,
        tool: String,
        stage: String,
    },

    #[snafu(display("missing tool `{tool}` on `{stage}`"))]
    MissingTool { tool: String, stage: String },

    #[snafu(display("runtime error on `{stage}`: {message}"))]
    Runtime { message: String, stage: String },

    #[snafu(display("tool `{tool}` failed on `{stage}`: {message}"))]
    ToolExecution {
        tool: String,
        message: String,
        stage: String,
    },

    #[snafu(display("tool `{tool}` requires approval before execution on `{stage}`"))]
    ToolApprovalRequired { tool: String, stage: String },

    #[snafu(display("tool `{tool}` IO error on `{stage}`: {source}"))]
    ToolIo {
        tool: String,
        stage: String,
        source: io::Error,
    },
}

/// Shared result alias for the extracted tools crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Returns the user-facing failure text that should be shown outside internal diagnostics.
    pub fn display_message(&self) -> String {
        match self {
            Self::Json { source, .. } => source.to_string(),
            Self::ToolTimeout { tool, .. } => format!("tool `{tool}` timed out"),
            Self::MissingTool { tool, .. } => format!("missing tool `{tool}`"),
            Self::Runtime { message, .. } => message.clone(),
            Self::ToolExecution { message, .. } => message.clone(),
            Self::ToolApprovalRequired { tool, .. } => {
                format!("tool `{tool}` requires approval before execution")
            }
            Self::ToolIo { source, .. } => source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    /// Verifies tool execution failures expose only their user-facing message.
    #[test]
    fn display_message_hides_internal_stage_for_tool_execution_failures() {
        let error = Error::ToolExecution {
            tool: "apply_patch".to_string(),
            message: "missing Begin/End markers".to_string(),
            stage: "apply-patch-parse".to_string(),
        };

        assert_eq!(error.display_message(), "missing Begin/End markers");
    }
}
