use serde::{Deserialize, Serialize};

use crate::{RunId, TimestampMs, TurnId};

/// Identifies one model turn within a higher-level agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnIdentity {
    /// Agent run that owns the turn.
    pub run_id: RunId,

    /// Stable turn identifier.
    pub turn_id: TurnId,
}

/// Complete persisted interval for one turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTiming {
    /// Time the turn began in Unix milliseconds.
    pub started_at_ms: TimestampMs,

    /// Time the final model or tool output ended in Unix milliseconds.
    pub ended_at_ms: TimestampMs,
}

/// Reports an invalid persisted turn interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TurnTimingError {
    /// The turn ended before it started.
    #[error("turn ended at {ended} before it started at {started}")]
    EndBeforeStart {
        /// Turn start timestamp.
        started: TimestampMs,

        /// Turn end timestamp.
        ended: TimestampMs,
    },
}

impl TryFrom<(TimestampMs, TimestampMs)> for TurnTiming {
    type Error = TurnTimingError;

    /// Builds complete turn timing while enforcing chronological ordering.
    fn try_from(
        (started_at_ms, ended_at_ms): (TimestampMs, TimestampMs),
    ) -> Result<Self, Self::Error> {
        if ended_at_ms < started_at_ms {
            return Err(TurnTimingError::EndBeforeStart {
                started: started_at_ms,
                ended: ended_at_ms,
            });
        }

        Ok(Self {
            started_at_ms,
            ended_at_ms,
        })
    }
}

/// Reason the model portion of a turn stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model completed without requesting another operation.
    EndTurn,
    /// The model requested a tool batch.
    ToolUse,
    /// The provider reached its output token limit.
    MaxTokens,
    /// The provider refused the request.
    Refusal,
    /// The model attempt ended with a provider or stream failure.
    Error,
    /// The model attempt was cancelled before a normal final result.
    Cancelled,
}

/// Final persisted outcome of a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnOutcome {
    /// Turn completed with a provider stop reason.
    Completed {
        /// Provider or kernel stop reason.
        stop_reason: StopReason,
    },

    /// Turn was cancelled before completion.
    Cancelled,

    /// Turn failed and records a human-readable diagnostic.
    Failed {
        /// Error summary persisted with the turn.
        message: String,
    },
}

/// Complete persisted turn record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecord {
    /// Run and turn identifiers flattened onto the persistence record.
    #[serde(flatten)]
    pub identity: TurnIdentity,

    /// Complete timing flattened onto the persistence record.
    #[serde(flatten)]
    pub timing: TurnTiming,

    /// Final turn outcome.
    pub outcome: TurnOutcome,
}
