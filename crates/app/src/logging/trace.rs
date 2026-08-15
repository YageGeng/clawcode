//! Type-safe Trace extraction from tracing span attributes.

use std::fmt;
use std::sync::Arc;

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::FormatFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

const TRACE_ID_FIELD: &str = "trace_id";

/// A Trace ID retained as typed span extension data for event formatting.
#[derive(Clone)]
pub(super) struct TraceContext(Arc<str>);

impl TraceContext {
    /// Extracts the Trace field from attributes recorded for a new span.
    fn from_attributes(attributes: &Attributes<'_>) -> Option<Self> {
        let mut visitor = TraceIdVisitor::default();
        attributes.record(&mut visitor);
        visitor.trace_id.map(|trace_id| Self(trace_id.into()))
    }

    /// Finds the root Trace inherited by the event's complete span scope.
    pub(super) fn current<S, N>(context: &FmtContext<'_, S, N>) -> Option<Self>
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        context.event_scope().and_then(|scope| {
            scope
                .from_root()
                .find_map(|span| span.extensions().get::<Self>().cloned())
        })
    }
}

impl fmt::Display for TraceContext {
    /// Writes the validated Trace value without adding field syntax.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Captures Trace fields when tracing creates spans.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TraceContextLayer;

impl<S> Layer<S> for TraceContextLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    /// Stores a Trace only on spans that explicitly declare `trace_id`.
    fn on_new_span(
        &self,
        attributes: &Attributes<'_>,
        id: &Id,
        context: Context<'_, S>,
    ) {
        let Some(trace) = TraceContext::from_attributes(attributes) else {
            return;
        };
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(trace);
        }
    }
}

/// Visitor restricted to the root Trace field used by the application.
#[derive(Default)]
struct TraceIdVisitor {
    trace_id: Option<String>,
}

impl Visit for TraceIdVisitor {
    /// Captures string fields without debug quotes.
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == TRACE_ID_FIELD {
            self.trace_id = Some(value.to_string());
        }
    }

    /// Captures display-recorded newtypes such as `protocol::TraceId`.
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == TRACE_ID_FIELD {
            self.trace_id = Some(format!("{:?}", value));
        }
    }
}
