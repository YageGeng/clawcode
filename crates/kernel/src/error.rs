use snafu::Snafu;

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

    #[snafu(display("tool `{tool}` timed out on `{stage}`, {source}"))]
    ToolTimeout {
        source: tokio::time::error::Elapsed,
        tool: String,
        stage: String,
    },

    #[snafu(display("missing prompt on `{stage}`"))]
    MissingPrompt { stage: String },

    #[snafu(display("missing tool `{tool}` on `{stage}`"))]
    MissingTool { tool: String, stage: String },

    #[snafu(display("session error on `{stage}`: {message}"))]
    Session { message: String, stage: String },

    #[snafu(display("runtime error on `{stage}`: {message}"))]
    Runtime { message: String, stage: String },

    #[snafu(display("tool `{tool}` failed on `{stage}`: {message}"))]
    ToolExecution {
        tool: String,
        message: String,
        stage: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
