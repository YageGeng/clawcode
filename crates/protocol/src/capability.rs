use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, EntryId, ModelUsage, RunId, ScalarError, TimestampMs, TurnId,
};

/// Maximum number of Unicode scalar values accepted in a session title.
const MAX_SESSION_TITLE_CHARS: usize = 120;

/// Immutable token-driven compaction settings aligned with pi v4.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct CompactionPolicy {
    /// Whether automatic threshold and overflow compaction are enabled.
    #[serde(default = "CompactionPolicy::default_enabled")]
    pub enabled: bool,
    /// Tokens reserved for the summary prompt and subsequent model output.
    #[serde(default = "CompactionPolicy::default_reserve_tokens")]
    pub reserve_tokens: u64,
    /// Approximate recent tokens retained verbatim after summarization.
    #[serde(default = "CompactionPolicy::default_keep_recent_tokens")]
    pub keep_recent_tokens: u64,
}

impl CompactionPolicy {
    /// Returns pi's default automatic compaction switch.
    const fn default_enabled() -> bool {
        true
    }

    /// Returns pi's default context reserve in tokens.
    const fn default_reserve_tokens() -> u64 {
        16_384
    }

    /// Returns pi's default recent-tail budget.
    const fn default_keep_recent_tokens() -> u64 {
        20_000
    }
}

impl Default for CompactionPolicy {
    /// Uses pi's enabled 16,384 reserve and 20,000 recent-token defaults.
    fn default() -> Self {
        Self::builder()
            .enabled(Self::default_enabled())
            .reserve_tokens(Self::default_reserve_tokens())
            .keep_recent_tokens(Self::default_keep_recent_tokens())
            .build()
    }
}

/// Assistant response retry policy with bounded exponential backoff.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct RetryPolicy {
    /// Whether transient assistant failures are retried.
    #[serde(default = "RetryPolicy::default_enabled")]
    pub enabled: bool,
    /// Number of retries beyond the initial assistant attempt.
    #[serde(default = "RetryPolicy::default_max_retries")]
    pub max_retries: u32,
    /// Initial retry delay in milliseconds before exponential growth.
    #[serde(default = "RetryPolicy::default_base_delay_ms")]
    pub base_delay_ms: u64,
}

impl RetryPolicy {
    /// Returns pi's default assistant retry switch.
    const fn default_enabled() -> bool {
        true
    }

    /// Returns pi's default number of assistant retries.
    const fn default_max_retries() -> u32 {
        3
    }

    /// Returns pi's default initial assistant retry delay.
    const fn default_base_delay_ms() -> u64 {
        2_000
    }
}

impl Default for RetryPolicy {
    /// Uses pi's enabled three-retry policy with a two-second base delay.
    fn default() -> Self {
        Self::builder()
            .enabled(Self::default_enabled())
            .max_retries(Self::default_max_retries())
            .base_delay_ms(Self::default_base_delay_ms())
            .build()
    }
}

/// Stable provider and model limits resolved once during application startup.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct ModelProfile {
    /// Configuration identifier of the selected provider.
    pub provider_id: String,
    /// Provider-facing identifier of the selected model.
    pub model_id: String,
    /// Human-readable model name used by protocol clients.
    pub display_name: String,
    /// Maximum model context size in tokens.
    pub context_tokens: u64,
    /// Maximum tokens the model may emit for one request.
    pub max_output_tokens: u64,
}

/// Cause that initiated a context compaction operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// A user or API caller explicitly requested compaction.
    Manual,
    /// Estimated usage crossed the configured compaction threshold.
    Threshold,
    /// The provider reported that the context window was exceeded.
    Overflow,
}

/// Validated, normalized display title persisted as a session name fact.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct SessionTitle(String);

impl SessionTitle {
    /// Returns the normalized session title as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionTitle {
    type Error = ScalarError;

    /// Trims and validates a title at the public protocol boundary.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let normalized = value.trim();
        if normalized.is_empty() {
            return Err(ScalarError::EmptySessionTitle);
        }
        if normalized.chars().count() > MAX_SESSION_TITLE_CHARS {
            return Err(ScalarError::SessionTitleTooLong {
                maximum: MAX_SESSION_TITLE_CHARS,
            });
        }

        Ok(Self(normalized.to_string()))
    }
}

impl TryFrom<&str> for SessionTitle {
    type Error = ScalarError;

    /// Copies and validates a borrowed session title.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_string())
    }
}

impl From<SessionTitle> for String {
    /// Consumes a normalized session title into its wire representation.
    fn from(value: SessionTitle) -> Self {
        value.0
    }
}

/// Correlation and exact timing required to rebuild a compaction summary message.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    /// Cause that produced this persisted compaction boundary.
    pub reason: CompactionReason,
    /// Run-shaped identifier shared by live and replay compaction events.
    pub run_id: RunId,
    /// Turn-shaped identifier shared by compaction events and summary context.
    pub turn_id: TurnId,
    /// Time summary generation began in Unix milliseconds.
    pub started_at_ms: TimestampMs,
    /// Time compaction persistence completed in Unix milliseconds.
    pub ended_at_ms: TimestampMs,
    /// Files read by tool calls represented in the summarized history.
    #[serde(default)]
    #[builder(default)]
    pub read_files: Vec<String>,
    /// Files created or changed by tool calls represented in the summarized history.
    #[serde(default)]
    #[builder(default)]
    pub modified_files: Vec<String>,
}

/// Exact pi v4 compaction payload persisted in a compaction entry.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct CompactionData {
    /// Model-generated summary of messages removed from direct context.
    pub summary: String,
    /// Complete recent messages retained after the summary.
    pub retained_tail: Vec<AgentMessage>,
    /// Estimated token count before compaction.
    #[serde(with = "crate::message::decimal_u64")]
    pub tokens_before: u64,
    /// Provider usage consumed while generating this summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub usage: Option<ModelUsage>,
    /// Optional producer-owned details preserved by pi v4 storage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub details: Option<CompactionDetails>,
}

/// Persisted identity and exact interval for one completed compaction.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    /// Entry containing the pi v4 compaction payload.
    pub entry_id: EntryId,
    /// Turn-shaped identifier attached to all compaction events.
    pub turn_id: TurnId,
    /// Structured checkpoint text rendered by ACP clients.
    pub summary: String,
    /// Effective context size immediately before compaction.
    #[serde(with = "crate::message::decimal_u64")]
    pub tokens_before: u64,
    /// Provider usage consumed while generating the summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub usage: Option<ModelUsage>,
    /// Time summary generation began in Unix milliseconds.
    pub started_at_ms: TimestampMs,
    /// Time persistence completed in Unix milliseconds.
    pub ended_at_ms: TimestampMs,
}

/// Typed terminal state for every started compaction operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CompactionOutcome {
    /// Summary generation and persistence completed successfully.
    Completed {
        /// Complete persisted result used by live and replay clients.
        result: CompactionResult,
    },
    /// Compaction failed with a stable client-facing message.
    Failed {
        /// Sanitized failure summary without provider internals.
        message: String,
    },
    /// The caller cancelled compaction before persistence completed.
    Cancelled,
}

/// Fixed operation kind encoded in a compaction intent record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionOperationKind {
    /// Context compaction operation.
    Compaction,
}

/// Typed intent persisted when one compaction operation starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionOperationIntent {
    kind: CompactionOperationKind,
    /// Optional user focus appended to the standard summary protocol.
    pub custom_instructions: Option<String>,
    /// Entry reserved for the completed compaction boundary.
    pub result_entry_id: EntryId,
}

impl CompactionOperationIntent {
    /// Creates the only valid intent kind for a compaction operation.
    #[must_use]
    pub fn new(
        custom_instructions: Option<String>,
        result_entry_id: EntryId,
    ) -> Self {
        Self {
            kind: CompactionOperationKind::Compaction,
            custom_instructions,
            result_entry_id,
        }
    }
}

/// Complete payload of a persisted compaction operation-start record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionOperationStarted {
    /// Active branch leaf before the result entry is appended.
    pub source_leaf_id: Option<EntryId>,
    /// Typed compaction intent and reserved result identity.
    pub intent: CompactionOperationIntent,
}

/// Fixed step name encoded in a compaction attempt record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStep {
    /// Model-backed context compaction step.
    Compaction,
}

/// Complete payload of one persisted compaction attempt.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStepAttempt {
    /// Fixed operation step discriminator.
    pub step: CompactionStep,
    /// One-based attempt number.
    pub attempt: u32,
    /// Entry reserved for the completed boundary.
    pub result_entry_id: EntryId,
    /// Trigger that requested compaction.
    pub compaction_reason: CompactionReason,
}

/// Fixed usage cause encoded in compaction accounting records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionUsageCause {
    /// Tokens consumed by checkpoint generation.
    Compaction,
}

/// Complete payload of compaction provider usage accounting.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct CompactionUsageRecord {
    /// Fixed compaction usage discriminator.
    pub cause: CompactionUsageCause,
    /// Completed boundary entry associated with the provider call.
    pub entry_id: EntryId,
    /// One-based summary attempt number.
    pub attempt: u32,
    /// Provider-normalized summary stop reason.
    pub stop_reason: crate::StopReason,
    /// Complete provider token accounting.
    pub usage: ModelUsage,
}

/// Persisted terminal state of a compaction operation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionOperationOutcome {
    /// The boundary entry was durably appended.
    Completed,
    /// Cancellation stopped summary generation.
    Aborted,
    /// Summary generation or persistence failed.
    Failed,
}

/// Internal diagnostic retained only in failed operation records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionOperationError {
    /// Stable machine-readable category.
    pub code: String,
    /// Complete internal error retained for local debugging.
    pub message: String,
}

/// Complete payload of a terminal compaction operation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionOperationFinished {
    /// Persisted terminal operation state.
    pub outcome: CompactionOperationOutcome,
    /// Failure diagnostics omitted for successful operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CompactionOperationError>,
}
