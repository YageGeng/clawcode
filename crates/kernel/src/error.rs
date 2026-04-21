use snafu::Snafu;

use crate::runtime::ToolCallRuntimeSnapshot;

/// Shared runtime error type for the first `kernel` milestone.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
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
        inflight_snapshot: Option<ToolCallRuntimeSnapshot>,
    },

    #[snafu(display("tool dispatch failed on `{stage}`, {source}"))]
    Tool {
        source: tools::Error,
        stage: String,
        inflight_snapshot: Option<ToolCallRuntimeSnapshot>,
    },

    #[snafu(display(
        "cleanup failed on `{stage}` after primary error `{source}`: {cleanup_error}"
    ))]
    Cleanup {
        source: Box<Error>,
        cleanup_error: Box<Error>,
        stage: String,
        inflight_snapshot: Option<ToolCallRuntimeSnapshot>,
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
                inflight_snapshot: Some(inflight_snapshot),
            },
            Self::Tool { source, stage, .. } => Self::Tool {
                source,
                stage,
                inflight_snapshot: Some(inflight_snapshot),
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
                inflight_snapshot: Some(inflight_snapshot),
            },
            other => other,
        }
    }
}
