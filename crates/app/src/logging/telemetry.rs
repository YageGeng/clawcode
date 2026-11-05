//! Safe local summaries of upstream Rig spans; no exporter or content recorder.

use std::{collections::BTreeMap, fmt, time::Instant};

use tracing::{
    Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// Owns only safe metadata for one model operation until its span closes.
pub(super) struct RigSpan {
    started: Instant,
    fields: BTreeMap<&'static str, String>,
}

impl RigSpan {
    /// Recognizes native Rig spans and the upstream completion-parent adoption marker.
    pub(super) fn accepts(metadata: &Metadata<'_>) -> bool {
        matches!(metadata.target(), "rig::completions" | "rig::modalities")
            || metadata.fields().field("rig.completion_parent").is_some()
    }

    /// Captures initial identity while excluding prompt, response, headers and native payloads.
    fn new(attributes: &Attributes<'_>) -> Self {
        let mut record = Self {
            started: Instant::now(),
            fields: BTreeMap::new(),
        };
        attributes.record(&mut record);
        record
    }

    /// Restricts arbitrary string/Debug values to the SDK's public identity fields.
    fn is_text_label(name: &str) -> bool {
        matches!(
            name,
            "gen_ai.operation.name"
                | "gen_ai.provider.name"
                | "gen_ai.request.model"
                | "gen_ai.response.model"
                | "gen_ai.response.id"
                | "rig.provider_request_id"
        )
    }
}

impl Visit for RigSpan {
    /// Quotes identity strings so embedded newlines cannot forge additional log lines.
    fn record_str(&mut self, field: &Field, value: &str) {
        if Self::is_text_label(field.name()) {
            // Preserve semantic-convention names and normalize the legacy Rig key.
            let name = if field.name() == "rig.provider_request_id" {
                "gen_ai.provider_request_id"
            } else {
                field.name()
            };
            self.fields.insert(name, format!("{value:?}"));
        }
    }

    /// Retains only native unsigned counters; absent fields remain absent rather than zero.
    fn record_u64(&mut self, field: &Field, value: u64) {
        // Keep an explicit allowlist so future SDK fields do not expand the
        // safe local summary without an intentional review.
        let name = match field.name() {
            "gen_ai.usage.input_tokens"
            | "gen_ai.usage.output_tokens"
            | "gen_ai.usage.cache_read.input_tokens"
            | "gen_ai.usage.cache_creation.input_tokens"
            | "gen_ai.usage.tool_use_prompt_tokens"
            | "gen_ai.usage.reasoning_tokens" => field.name(),
            _ => return,
        };
        self.fields.insert(name, value.to_string());
    }

    /// Accepts nonnegative integer literals on adopted parent spans without inventing counts.
    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(value) = u64::try_from(value) {
            self.record_u64(field, value);
        }
    }

    /// Records delivery mode without accepting unrelated boolean fields.
    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "gen_ai.request.stream" {
            self.fields.insert("stream", value.to_string());
        }
    }

    /// Ignores arbitrary content fields even when a caller opts into SDK content telemetry.
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if Self::is_text_label(field.name()) {
            self.record_str(field, &format!("{value:?}"));
        }
    }
}

impl RigSpan {
    /// Writes the collected fields without adding a lifecycle-specific prefix.
    pub(super) fn write_fields<W: fmt::Write>(
        &self,
        formatter: &mut W,
    ) -> fmt::Result {
        for (name, value) in &self.fields {
            write!(formatter, " {name}={value}")?;
        }
        Ok(())
    }
}

impl fmt::Display for RigSpan {
    /// Prints one lifecycle summary without treating cancellation/error closure as success.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "model request done; elapsed_ms={}",
            self.started.elapsed().as_millis()
        )?;
        self.write_fields(formatter)
    }
}

/// Keeps metadata in its own span; the existing formatter handles output and Trace correlation.
pub(super) struct ModelTracingLayer;

impl<S> Layer<S> for ModelTracingLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    /// Allocates a record only for model spans, without per-token observations or global maps.
    fn on_new_span(
        &self,
        attributes: &Attributes<'_>,
        id: &Id,
        context: Context<'_, S>,
    ) {
        if RigSpan::accepts(attributes.metadata())
            && let Some(span) = context.span(id)
        {
            span.extensions_mut().insert(RigSpan::new(attributes));
        }
    }

    /// Merges late response identity and usage into the operation's final local summary.
    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && RigSpan::accepts(span.metadata())
            && let Some(record) = span.extensions_mut().get_mut::<RigSpan>()
        {
            values.record(record);
        }
    }
}
