use std::path::PathBuf;

use snafu::Snafu;

/// Structured errors raised while reading or writing persisted session data.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("store I/O failed on `{stage}` for `{}`: {source}", path.display()))]
    Io {
        source: std::io::Error,
        stage: String,
        path: PathBuf,
    },

    #[snafu(display("store JSON failed on `{stage}` for `{}`: {source}", path.display()))]
    Json {
        source: serde_json::Error,
        stage: String,
        path: PathBuf,
    },

    #[snafu(display("store task failed on `{stage}` for `{}`: {source}", path.display()))]
    Join {
        source: tokio::task::JoinError,
        stage: String,
        path: PathBuf,
    },
}

/// Shared result alias for store operations that can fail structurally.
pub type Result<T, E = Error> = std::result::Result<T, E>;
