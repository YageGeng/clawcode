//! Transport-neutral ACP request logging.
//!
//! ACP requests are not traces: a trace is one Kernel Run, which the Kernel
//! opens itself. This module only records each request's lifecycle at the level
//! its kind deserves. The module path stays `acp::trace` so existing filter
//! directives such as `acp::trace=debug` keep selecting these lines.

mod parameters;

use std::fmt;
use std::future::Future;
use std::time::Instant;

use agent_client_protocol::{JsonRpcResponse, v2::Responder};
use protocol::AcpExtensionMethod;
use serde::Serialize;

use self::parameters::AcpLogParameters;

/// Standard ACP method that only reads the Session list.
const SESSION_LIST_METHOD: &str = "session/list";

/// Transport family through which an ACP operation entered the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpTransportKind {
    /// ACP JSON-RPC framed through process stdin and stdout.
    Stdio,
    /// ACP HTTP, SSE, or WebSocket traffic served by the official HTTP transport.
    Http,
}

impl fmt::Display for AcpTransportKind {
    /// Writes the stable transport label used in readable log messages.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => formatter.write_str("stdio"),
            Self::Http => formatter.write_str("http"),
        }
    }
}

/// Whether a request only reads a snapshot or changes state, which sets its log level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpRequestKind {
    /// Read-only snapshot: lifecycle lines only at DEBUG.
    Query,
    /// Anything else: one INFO line when it completes.
    Change,
}

impl From<&str> for AcpRequestKind {
    /// Classifies one JSON-RPC method name.
    fn from(method: &str) -> Self {
        let query = method == SESSION_LIST_METHOD
            || AcpExtensionMethod::parse(method)
                .is_some_and(AcpExtensionMethod::is_query);
        if query { Self::Query } else { Self::Change }
    }
}

impl AcpTransportKind {
    /// Starts the log of one JSON-RPC request received over this transport.
    pub(crate) fn request_log<T, P>(
        &self,
        responder: &Responder<T>,
        parameters: &P,
    ) -> AcpRequestLog
    where
        T: JsonRpcResponse,
        P: Serialize + ?Sized,
    {
        AcpRequestLog::new(
            format!(
                "ACP request {}:{} over {}",
                responder.method(),
                responder.id(),
                self
            ),
            AcpLogParameters::capture(parameters),
            AcpRequestKind::from(responder.method()),
        )
    }

    /// Starts the log of one notification received over this transport; it has no request id.
    pub(crate) fn notification_log<P>(
        &self,
        method: &str,
        parameters: &P,
    ) -> AcpRequestLog
    where
        P: Serialize + ?Sized,
    {
        AcpRequestLog::new(
            format!("ACP notification {} over {}", method, self),
            AcpLogParameters::capture(parameters),
            AcpRequestKind::from(method),
        )
    }
}

/// What one request is, kept across the detached tasks that finish it.
#[derive(Clone)]
struct AcpRequestIdentity {
    description: String,
    parameters: AcpLogParameters,
    kind: AcpRequestKind,
}

/// Lifecycle log of one ACP request; it opens no span and owns no trace.
#[derive(Clone)]
pub(crate) struct AcpRequestLog {
    identity: AcpRequestIdentity,
    started_at: Instant,
}

impl AcpRequestLog {
    /// Records when the request arrived.
    fn new(
        description: String,
        parameters: AcpLogParameters,
        kind: AcpRequestKind,
    ) -> Self {
        Self {
            identity: AcpRequestIdentity {
                description,
                parameters,
                kind,
            },
            started_at: Instant::now(),
        }
    }

    /// Logs that the request entered application handling; always DEBUG.
    pub(crate) fn start(&self) {
        tracing::debug!("started {}", self.identity.description);
        self.identity.parameters.log(&self.identity.description);
    }

    /// Continues an already-started request until its work settles, then logs the outcome.
    pub(crate) async fn settle<F, T, E>(self, future: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        let result = future.await;
        self.log_settlement(&result);
        result
    }

    /// Runs one complete request and logs its start and settlement.
    pub(crate) async fn run<F, T, E>(self, future: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        self.start();
        self.settle(future).await
    }

    /// Logs a setup failure for a request that could not start detached work.
    pub(crate) fn fail<E>(&self, error: &E)
    where
        E: fmt::Display,
    {
        tracing::warn!(
            "failed {} in {} ms: {}",
            self.identity.description,
            self.started_at.elapsed().as_millis(),
            error
        );
    }

    /// Writes one completion line at the level of the request's kind, or a WARN on failure.
    fn log_settlement<T, E>(&self, result: &Result<T, E>)
    where
        E: fmt::Display,
    {
        let elapsed_ms = self.started_at.elapsed().as_millis();
        match (result, self.identity.kind) {
            (Ok(_), AcpRequestKind::Query) => tracing::debug!(
                "completed {} in {} ms",
                self.identity.description,
                elapsed_ms
            ),
            (Ok(_), AcpRequestKind::Change) => tracing::info!(
                "completed {} in {} ms",
                self.identity.description,
                elapsed_ms
            ),
            (Err(error), _) => self.fail(error),
        }
    }
}
