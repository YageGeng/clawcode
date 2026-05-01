use std::path::PathBuf;

use snafu::Snafu;

use crate::runtime::ToolCallRuntimeSnapshot;

/// Shared runtime error type for the first `kernel` milestone.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("I/O error on `{stage}`, {source}"))]
    Io {
        source: std::io::Error,
        stage: String,
    },

    #[snafu(display("I/O error on `{stage}` for `{}`: {source}", path.display()))]
    PathIo {
        source: std::io::Error,
        stage: String,
        path: PathBuf,
    },

    #[snafu(display("channel closed on `{stage}`, {source}"))]
    Channel {
        source: tokio::sync::watch::error::RecvError,
        stage: String,
    },

    #[snafu(display("model error on `{stage}`, {source}"))]
    Model {
        source: llm::completion::CompletionError,
        stage: String,
    },

    #[snafu(display("json error on `{stage}`, {source}"))]
    Json {
        source: serde_json::Error,
        stage: String,
    },

    #[snafu(display("missing prompt on `{stage}`"))]
    MissingPrompt { stage: String },

    #[snafu(display("runtime error on `{stage}`: {message}"))]
    Runtime {
        message: String,
        stage: String,
        inflight_snapshot: Option<Box<ToolCallRuntimeSnapshot>>,
    },

    #[snafu(display("tool dispatch failed on `{stage}`, {source}"))]
    Tool {
        #[snafu(source(from(tools::Error, Box::new)))]
        source: Box<tools::Error>,
        stage: String,
        inflight_snapshot: Option<Box<ToolCallRuntimeSnapshot>>,
    },

    #[snafu(display("skill error on `{stage}`, {source}"))]
    Skills {
        source: skills::Error,
        stage: String,
    },

    #[snafu(display(
        "cleanup failed on `{stage}` after primary error `{source}`: {cleanup_error}"
    ))]
    Cleanup {
        source: Box<Error>,
        cleanup_error: Box<Error>,
        stage: String,
        inflight_snapshot: Option<Box<ToolCallRuntimeSnapshot>>,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Attaches the latest tool-call runtime snapshot to errors that can surface turn execution state.
    pub fn with_inflight_snapshot(self, inflight_snapshot: ToolCallRuntimeSnapshot) -> Self {
        match self {
            Self::Runtime { message, stage, .. } => Self::Runtime {
                message,
                stage,
                inflight_snapshot: Some(Box::new(inflight_snapshot)),
            },
            Self::Tool { source, stage, .. } => Self::Tool {
                source,
                stage,
                inflight_snapshot: Some(Box::new(inflight_snapshot)),
            },
            Self::Cleanup {
                source,
                cleanup_error,
                stage,
                ..
            } => Self::Cleanup {
                source,
                cleanup_error,
                stage,
                inflight_snapshot: Some(Box::new(inflight_snapshot)),
            },
            other => other,
        }
    }

    /// Returns the user-facing failure text that should be surfaced to models and CLI clients.
    pub fn display_message(&self) -> String {
        match self {
            Self::Tool { source, .. } => source.display_message().into_owned(),
            _ => self.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    /// Verifies kernel tool errors delegate user-facing text to the inner tool error.
    #[test]
    fn display_message_uses_inner_tool_error_message() {
        let error = Error::Tool {
            source: Box::new(tools::Error::ToolExecution {
                tool: "apply_patch".to_string(),
                message: "missing Begin/End markers".to_string(),
                stage: "apply-patch-parse".to_string(),
            }),
            stage: "dispatch-tool".to_string(),
            inflight_snapshot: None,
        };

        assert_eq!(error.display_message(), "missing Begin/End markers");
    }
}
