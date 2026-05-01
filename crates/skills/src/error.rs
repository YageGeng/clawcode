use std::path::PathBuf;

use snafu::Snafu;

/// Hard failures raised while building prompt-visible skill injections.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("skill IO error on `{stage}` for `{}`: {source}", path.display()))]
    Io {
        source: std::io::Error,
        stage: String,
        path: PathBuf,
    },

    #[snafu(display("skill parse error on `{stage}` for `{}`: {message}", path.display()))]
    Parse {
        message: String,
        stage: String,
        path: PathBuf,
    },
}

/// Result alias used by operations that cannot safely degrade to load warnings.
pub type Result<T, E = Error> = std::result::Result<T, E>;
