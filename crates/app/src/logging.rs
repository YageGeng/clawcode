//! Process-wide tracing subscriber construction.

mod format;
mod trace;

use config::LoggingConfig;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

use self::format::CompactEventFormat;
use self::trace::TraceContextLayer;

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
    /// Parses an explicit tracing filter and retains its ANSI color policy.
    pub fn new(
        filter: impl AsRef<str>,
        color: bool,
    ) -> Result<Self, LoggingError> {
        Ok(Self {
            filter: EnvFilter::try_new(filter.as_ref())?,
            color,
        })
    }

    /// Reads `RUST_LOG` once while retaining the immutable TOML color policy.
    pub fn from_config(config: &LoggingConfig) -> Result<Self, LoggingError> {
        match std::env::var(RUST_LOG_ENV) {
            Ok(filter) if !filter.trim().is_empty() => {
                Self::new(filter, config.color)
            }
            Ok(_) | Err(std::env::VarError::NotPresent) => {
                Self::new(&config.filter, config.color)
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(LoggingError::InvalidEnvironment)
            }
        }
    }

    /// Builds a filtered formatter with the supplied destination writer.
    pub fn build<W>(self, writer: W) -> impl tracing::Subscriber + Send + Sync
    where
        W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
    {
        let Self { filter, color } = self;
        let formatter = tracing_subscriber::fmt::layer()
            .event_format(CompactEventFormat::default())
            .with_writer(writer)
            .with_ansi(color);
        tracing_subscriber::registry()
            .with(filter)
            .with(TraceContextLayer)
            .with(formatter)
    }

    /// Installs a stderr subscriber so stdio stdout remains valid ACP framing.
    pub fn install(self) -> Result<(), LoggingError> {
        tracing::subscriber::set_global_default(self.build(std::io::stderr))?;
        Ok(())
    }
}
