//! Transport-neutral ACP operation tracing.

mod parameters;

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use agent_client_protocol::{JsonRpcResponse, Responder};
use protocol::{IdGenerator, IdKind, TraceId};
use serde::Serialize;

use self::parameters::AcpLogParameters;

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

/// Creates independent operation traces while sharing the application ID policy.
#[derive(Clone)]
pub(crate) struct AcpTraceFactory {
    id_generator: Arc<dyn IdGenerator>,
    transport: AcpTransportKind,
}

impl AcpTraceFactory {
    /// Binds Trace generation to one concrete ACP transport family.
    pub(crate) fn new(
        id_generator: Arc<dyn IdGenerator>,
        transport: AcpTransportKind,
    ) -> Self {
        Self {
            id_generator,
            transport,
        }
    }

    /// Creates a root Trace from one incoming JSON-RPC request context.
    pub(crate) fn request<T, P>(
        &self,
        responder: &Responder<T>,
        parameters: &P,
    ) -> Result<AcpOperationTrace, agent_client_protocol::Error>
    where
        T: JsonRpcResponse,
        P: Serialize + ?Sized,
    {
        self.operation(
            format!(
                "ACP request {}:{} over {}",
                responder.method(),
                responder.id(),
                self.transport
            ),
            AcpLogParameters::capture(parameters),
        )
    }

    /// Creates a root Trace for an incoming notification without a request ID.
    pub(crate) fn notification<P>(
        &self,
        method: &str,
        parameters: &P,
    ) -> Result<AcpOperationTrace, agent_client_protocol::Error>
    where
        P: Serialize + ?Sized,
    {
        self.operation(
            format!("ACP notification {} over {}", method, self.transport),
            AcpLogParameters::capture(parameters),
        )
    }

    /// Validates the generated Trace ID before constructing its root span.
    fn operation(
        &self,
        description: String,
        parameters: AcpLogParameters,
    ) -> Result<AcpOperationTrace, agent_client_protocol::Error> {
        let trace_id = TraceId::try_from(self.id_generator.next(IdKind::Trace))
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        let span = tracing::info_span!("acp_operation", trace_id = %trace_id);
        Ok(AcpOperationTrace {
            identity: AcpOperationIdentity {
                description,
                parameters,
            },
            trace_id,
            span,
            started_at: Instant::now(),
        })
    }
}

/// Human-readable identity retained across detached operation tasks.
#[derive(Clone)]
struct AcpOperationIdentity {
    description: String,
    parameters: AcpLogParameters,
}

/// Root span and readable lifecycle description for one ACP operation.
#[derive(Clone)]
pub(crate) struct AcpOperationTrace {
    identity: AcpOperationIdentity,
    trace_id: TraceId,
    span: tracing::Span,
    started_at: Instant,
}

impl AcpOperationTrace {
    /// Returns the typed ingress Trace propagated into Kernel request contexts.
    pub(crate) fn trace_id(&self) -> &TraceId {
        &self.trace_id
    }

    /// Logs that the operation entered application handling under its root span.
    pub(crate) fn start(&self) {
        self.span.in_scope(|| {
            // The process formatter reads Trace ID from the active span, so
            // lifecycle text must not duplicate the same identifier.
            tracing::info!("started {}", self.identity.description);
            self.identity.parameters.log(&self.identity.description);
        });
    }

    /// Polls a future under this Trace without settling its lifecycle.
    pub(crate) async fn instrument<F>(&self, future: F) -> F::Output
    where
        F: Future,
    {
        tracing::Instrument::instrument(future, self.span.clone()).await
    }

    /// Continues an already-started operation until its detached work settles.
    pub(crate) async fn settle<F, T, E>(self, future: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        let span = self.span.clone();
        tracing::Instrument::instrument(
            async move {
                let result = future.await;
                self.log_settlement(&result);
                result
            },
            span,
        )
        .await
    }

    /// Runs one complete operation under its root span and logs its settlement.
    pub(crate) async fn run<F, T, E>(self, future: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        self.start();
        self.settle(future).await
    }

    /// Logs a setup failure for an operation that could not start detached work.
    pub(crate) fn fail<E>(&self, error: &E)
    where
        E: fmt::Display,
    {
        self.span.in_scope(|| {
            tracing::warn!(
                "failed {} in {} ms: {}",
                self.identity.description,
                self.started_at.elapsed().as_millis(),
                error
            );
        });
    }

    /// Writes one readable success or failure event after operation settlement.
    fn log_settlement<T, E>(&self, result: &Result<T, E>)
    where
        E: fmt::Display,
    {
        match result {
            Ok(_) => tracing::info!(
                "completed {} in {} ms",
                self.identity.description,
                self.started_at.elapsed().as_millis()
            ),
            Err(error) => tracing::warn!(
                "failed {} in {} ms: {}",
                self.identity.description,
                self.started_at.elapsed().as_millis(),
                error
            ),
        }
    }
}
