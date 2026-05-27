//! Logging setup for ACP entrypoints.

use std::fs::OpenOptions;
use std::path::PathBuf;

use chrono::{Datelike, Local, NaiveDate};

/// Initialize ACP logging to the default daily log file.
///
/// # Errors
///
/// Returns an error when the log directory or log file cannot be created.
pub fn init_logging() -> std::io::Result<()> {
    let path = default_log_file_path(Local::now().date_naive())?;
    init_file_logging(path)
}

/// Build the default log file path for a local calendar date.
fn default_log_file_path(date: NaiveDate) -> std::io::Result<PathBuf> {
    let home_dir = dirs::home_dir().ok_or_else(|| {
        std::io::Error::other("failed to resolve home directory")
    })?;
    let file_name = format!(
        "{:04}-{:02}-{:02}.log",
        date.year(),
        date.month(),
        date.day()
    );

    Ok(home_dir
        .join(".local/share")
        .join("clawcode")
        .join("log")
        .join(file_name))
}

/// Initialize tracing to append logs to the provided file path.
fn init_file_logging(path: PathBuf) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::other("log path has no parent directory")
    })?;
    std::fs::create_dir_all(parent)?;
    let file = OpenOptions::new().create(true).append(true).open(path)?;

    // Ignore repeated initialization so tests or embedding binaries that already
    // installed a subscriber do not fail when starting the in-process ACP server.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::sync::Mutex::new(file))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .try_init()
        .ok();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the default log path uses the requested daily filename.
    #[test]
    fn default_log_file_path_uses_daily_file_name() {
        let path = default_log_file_path(
            NaiveDate::from_ymd_opt(2026, 5, 17)
                .expect("test date should be valid"),
        )
        .expect("log path should resolve");

        assert!(path.ends_with("clawcode/log/2026-05-17.log"));
    }
}
