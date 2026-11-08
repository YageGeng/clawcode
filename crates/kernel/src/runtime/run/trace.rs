//! Run tracing: where a Run's trace comes from and the span that carries it.
//!
//! A trace is one top-level Run. Every place that mints a `RunId` opens one
//! `run` span through `RunSpan`; nothing else in the workspace creates it.

use std::future::Future;

use protocol::{
    IdGenerator, IdKind, RUN_ID_FIELD, RunId, TRACE_ID_FIELD, TraceId,
};

use crate::runtime::KernelError;

/// Where a new top-level Run's trace comes from.
#[derive(Debug, Clone, Default)]
pub enum TraceOrigin {
    /// Foreground subagent: the child Run continues the caller's trace.
    Inherit(TraceId),
    /// Background subagent: a new trace whose start line names the caller's trace.
    LinkFrom(TraceId),
    /// Any other Run: a new trace.
    #[default]
    New,
}

impl TraceOrigin {
    /// Resolves the trace the Run declares: the caller's when inherited, otherwise a new one.
    pub(crate) fn trace_id(
        &self,
        ids: &dyn IdGenerator,
    ) -> Result<TraceId, KernelError> {
        match self {
            Self::Inherit(trace_id) => Ok(trace_id.clone()),
            Self::LinkFrom(_) | Self::New => {
                TraceId::try_from(ids.next(IdKind::Trace))
                    .map_err(|error| KernelError::Protocol(error.to_string()))
            }
        }
    }
}

/// The span of one Run, which is the only kind of span that carries correlation.
///
/// It is created at ERROR level because filters enable spans by level: an INFO
/// span would vanish under `filter = "warn"` and take the prefix of every warning
/// in the Run with it. The span writes no events of its own, so the level only
/// decides that it exists.
pub struct RunSpan(tracing::Span);

impl RunSpan {
    /// Opens the span of a top-level Run, which declares its trace.
    #[must_use]
    pub fn top_level(trace_id: &TraceId, run_id: &RunId) -> Self {
        Self(tracing::error_span!(
            "run",
            { TRACE_ID_FIELD } = %trace_id,
            { RUN_ID_FIELD } = %run_id
        ))
    }

    /// Opens the span of a Run nested in another Run; the enclosing span supplies the trace.
    #[must_use]
    pub fn nested(run_id: &RunId) -> Self {
        Self(tracing::error_span!("run", { RUN_ID_FIELD } = %run_id))
    }

    /// Runs one operation inside this span, including every poll and its drop.
    pub async fn instrument<F: Future>(self, future: F) -> F::Output {
        tracing::Instrument::instrument(future, self.0).await
    }
}
