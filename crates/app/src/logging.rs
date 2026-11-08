//! Process-wide tracing subscriber construction.

use std::io::IsTerminal;

use config::LoggingConfig;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

const RUST_LOG_ENV: &str = "RUST_LOG";

/// Errors surfaced while parsing or installing immutable logging configuration.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    /// `RUST_LOG` contained non-Unicode data and cannot be interpreted safely.
    #[error("RUST_LOG is not valid Unicode")]
    InvalidEnvironment,
    /// The selected tracing filter directive was invalid.
    #[error("invalid tracing filter: {0}")]
    InvalidFilter(#[from] tracing_subscriber::filter::ParseError),
    /// Another process component installed a global subscriber first.
    #[error("global tracing subscriber is already installed: {0}")]
    AlreadyInstalled(#[from] tracing::subscriber::SetGlobalDefaultError),
}

/// Builds one immutable tracing subscriber for the complete process.
#[derive(Debug)]
pub struct LoggingFactory {
    filter: EnvFilter,
    color: bool,
}

impl LoggingFactory {
    /// Parses an explicit tracing filter; `color: false` turns ANSI color off.
    pub fn new(
        filter: impl AsRef<str>,
        color: bool,
    ) -> Result<Self, LoggingError> {
        Ok(Self {
            filter: EnvFilter::try_new(filter.as_ref())?,
            color,
        })
    }

    /// Reads `RUST_LOG` once; color is allowed only when stderr is a terminal,
    /// so redirected logs stay free of ANSI escape codes.
    pub fn from_config(config: &LoggingConfig) -> Result<Self, LoggingError> {
        let color = config.color && std::io::stderr().is_terminal();
        match std::env::var(RUST_LOG_ENV) {
            Ok(filter) if !filter.trim().is_empty() => Self::new(filter, color),
            Ok(_) | Err(std::env::VarError::NotPresent) => {
                Self::new(&config.filter, color)
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(LoggingError::InvalidEnvironment)
            }
        }
    }

    /// Builds tracing's default formatter with the filter and destination writer.
    pub fn build<W>(self, writer: W) -> impl tracing::Subscriber + Send + Sync
    where
        W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
    {
        let Self { filter, color } = self;
        let builder = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer);
        // tracing colors output unless `NO_COLOR` is set; the setting can only turn it off.
        if color {
            builder.finish()
        } else {
            builder.with_ansi(false).finish()
        }
    }

    /// Installs a stderr subscriber so stdio stdout remains valid ACP framing.
    pub fn install(self) -> Result<(), LoggingError> {
        tracing::subscriber::set_global_default(self.build(std::io::stderr))?;
        Ok(())
    }
}
