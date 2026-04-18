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
