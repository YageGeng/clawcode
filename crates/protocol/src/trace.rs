//! Span field names of the Kernel's Run spans.
//!
//! The Kernel declares these fields when it opens a Run span, tracing's
//! formatter prints them as `run{trace_id=… run_id=…}`, and tests read them
//! back, so every side names them only here.

/// Span field carrying the trace of the top-level Run.
pub const TRACE_ID_FIELD: &str = "trace_id";

/// Span field carrying the id of the Run the span belongs to.
pub const RUN_ID_FIELD: &str = "run_id";
