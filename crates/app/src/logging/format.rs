//! Compact human-readable tracing event formatting.

use std::fmt;
use std::path::Path;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::field::VisitOutput;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{
    DefaultVisitor, FormatEvent, FormatFields, Writer,
};
use tracing_subscriber::fmt::time::{ChronoLocal, FormatTime};
use tracing_subscriber::registry::LookupSpan;

use super::trace::TraceContext;

const LOCAL_TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";
const ANSI_RESET: &str = "\u{1b}[0m";
const ANSI_TRACE: &str = "\u{1b}[35m";
const ANSI_DEBUG: &str = "\u{1b}[34m";
const ANSI_INFO: &str = "\u{1b}[32m";
const ANSI_WARN: &str = "\u{1b}[33m";
const ANSI_ERROR: &str = "\u{1b}[31m";
const ACP_INCOMING_ACTOR_TARGET: &str =
    "agent_client_protocol::jsonrpc::incoming_actor";

/// Formats one event without rendering the complete tracing span chain.
pub(super) struct CompactEventFormat {
    timer: ChronoLocal,
}

impl Default for CompactEventFormat {
    /// Uses local wall-clock time with millisecond precision.
    fn default() -> Self {
        Self {
            timer: ChronoLocal::new(LOCAL_TIMESTAMP_FORMAT.to_string()),
        }
    }
}

impl<S, N> FormatEvent<S, N> for CompactEventFormat
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    /// Writes timestamp, level, Trace, source location, and event fields once.
    fn format_event(
        &self,
        context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        self.timer.format_time(&mut writer)?;
        // A custom formatter must consume the ANSI capability exposed by the
        // layer; `with_ansi` does not style custom event output automatically.
        if writer.has_ansi_escapes() {
            let color = match *event.metadata().level() {
                tracing::Level::TRACE => ANSI_TRACE,
                tracing::Level::DEBUG => ANSI_DEBUG,
                tracing::Level::INFO => ANSI_INFO,
                tracing::Level::WARN => ANSI_WARN,
                tracing::Level::ERROR => ANSI_ERROR,
            };
            write!(
                writer,
                " {}{:<5}{} [",
                color,
                event.metadata().level(),
                ANSI_RESET
            )?;
        } else {
            write!(writer, " {:<5} [", event.metadata().level())?;
        }
        match TraceContext::current(context) {
            Some(trace) => write!(writer, "{}", trace)?,
            None => write!(writer, "-")?,
        }
        write!(writer, "] {} | ", EventSource::from(event.metadata()))?;
        if event.metadata().target() == ACP_INCOMING_ACTOR_TARGET {
            // The ACP SDK recursively Debug-formats its handler chain, so the
            // final field can grow into an unreadable wall of escaped slashes.
            let mut fields = AcpFields {
                inner: DefaultVisitor::new(writer.by_ref(), true),
            };
            event.record(&mut fields);
            fields.inner.finish()?;
        } else {
            context.format_fields(writer.by_ref(), event)?;
        }
        writeln!(writer)
    }
}

/// Formats ACP incoming-actor fields while suppressing handler-chain noise.
struct AcpFields<'writer> {
    inner: DefaultVisitor<'writer>,
}

impl Visit for AcpFields<'_> {
    /// Replaces the recursively escaped handler chain with a fixed marker.
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "handler" {
            self.inner.record_debug(field, &format_args!("<omitted>"));
        } else {
            self.inner.record_debug(field, value);
        }
    }

    /// Preserves the default formatter's unquoted event-message rendering.
    fn record_str(&mut self, field: &Field, value: &str) {
        self.inner.record_str(field, value);
    }

    /// Preserves the default formatter's error-source rendering.
    fn record_error(
        &mut self,
        field: &Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.inner.record_error(field, value);
    }
}

/// Source metadata rendered without leaking absolute dependency cache paths.
struct EventSource<'metadata> {
    file: Option<&'metadata str>,
    line: Option<u32>,
    target: &'metadata str,
}

impl<'metadata> From<&'metadata tracing::Metadata<'_>>
    for EventSource<'metadata>
{
    /// Captures source metadata from one tracing callsite.
    fn from(metadata: &'metadata tracing::Metadata<'_>) -> Self {
        Self {
            file: metadata.file(),
            line: metadata.line(),
            target: metadata.target(),
        }
    }
}

impl fmt::Display for EventSource<'_> {
    /// Writes a workspace-relative path or dependency filename with its line.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.file {
            Some(file) if Path::new(file).is_absolute() => {
                let filename = Path::new(file)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(file);
                formatter.write_str(filename)?;
            }
            Some(file) => formatter.write_str(file)?,
            None => formatter.write_str(self.target)?,
        }
        if let Some(line) = self.line {
            write!(formatter, ":{}", line)?;
        }
        Ok(())
    }
}
