use std::fmt;

use serde::{Deserialize, Serialize};

/// Largest integer that JavaScript can represent without precision loss.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Reports invalid scalar values received at protocol boundaries.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScalarError {
    /// A domain identifier was empty or contained only whitespace.
    #[error("{kind} must not be empty")]
    EmptyIdentifier { kind: &'static str },

    /// A timestamp was not an unsigned decimal millisecond value.
    #[error("timestamp must be an unsigned decimal millisecond string")]
    InvalidTimestamp,

    /// A sequence was zero or exceeded JavaScript's safe integer range.
    #[error("sequence must be between 1 and {MAX_SAFE_INTEGER}")]
    InvalidSequence,

    /// A session title was empty after trimming whitespace.
    #[error("session title must not be empty")]
    EmptySessionTitle,

    /// A session title exceeded the public Unicode scalar limit.
    #[error("session title must not exceed {maximum} characters")]
    SessionTitleTooLong { maximum: usize },
}

macro_rules! define_string_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("A validated ", $kind, " identifier.")]
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Returns the ", $kind, " identifier as a string slice.")]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ScalarError;

            #[doc = concat!("Validates and constructs a ", $kind, " identifier.")]
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.trim().is_empty() {
                    return Err(ScalarError::EmptyIdentifier { kind: $kind });
                }

                Ok(Self(value))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ScalarError;

            #[doc = concat!("Validates and copies a borrowed ", $kind, " identifier.")]
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_owned())
            }
        }

        impl From<$name> for String {
            #[doc = concat!("Consumes the ", $kind, " identifier into its wire value.")]
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl AsRef<str> for $name {
            #[doc = concat!("Borrows the ", $kind, " identifier as a string slice.")]
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            #[doc = concat!("Writes the ", $kind, " identifier without decoration.")]
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

define_string_id!(SessionId, "session");
define_string_id!(TraceId, "trace");
define_string_id!(RunId, "run");
define_string_id!(TurnId, "turn");
define_string_id!(MessageId, "message");
define_string_id!(ToolCallId, "tool call");
define_string_id!(EntryId, "entry");
define_string_id!(LaneId, "lane");
define_string_id!(RecordId, "record");
define_string_id!(QueueId, "queue");
define_string_id!(ExtensionId, "extension");

impl TurnId {
    /// Creates the stable synthetic Turn identity for out-of-turn session messages.
    #[must_use]
    pub fn system(session_id: &SessionId) -> Self {
        Self(format!("system-{session_id}"))
    }
}

/// Unix time in milliseconds serialized as a decimal string.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct TimestampMs(u64);

impl TimestampMs {
    /// Returns the parsed millisecond value for arithmetic and ordering.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<String> for TimestampMs {
    type Error = ScalarError;

    /// Parses an unsigned decimal millisecond string without accepting signs or whitespace.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(ScalarError::InvalidTimestamp);
        }

        value
            .parse::<u64>()
            .map(Self)
            .map_err(|_parse_error| ScalarError::InvalidTimestamp)
    }
}

impl TryFrom<&str> for TimestampMs {
    type Error = ScalarError;

    /// Parses a borrowed unsigned decimal millisecond string.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl From<TimestampMs> for String {
    /// Converts the timestamp to its precision-safe wire representation.
    fn from(value: TimestampMs) -> Self {
        value.0.to_string()
    }
}

impl From<u64> for TimestampMs {
    /// Wraps an unsigned clock value as Unix milliseconds.
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for TimestampMs {
    /// Writes the timestamp as unsigned decimal milliseconds.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Monotonic event sequence constrained to JavaScript's safe integer range.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(try_from = "u64", into = "u64")]
pub struct Sequence(u64);

impl Sequence {
    /// Returns the validated numeric sequence.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for Sequence {
    type Error = ScalarError;

    /// Validates a non-zero JavaScript-safe event sequence.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value == 0 || value > MAX_SAFE_INTEGER {
            return Err(ScalarError::InvalidSequence);
        }

        Ok(Self(value))
    }
}

impl From<Sequence> for u64 {
    /// Converts a validated sequence into its numeric wire value.
    fn from(value: Sequence) -> Self {
        value.0
    }
}

impl fmt::Display for Sequence {
    /// Writes the event sequence as an unsigned integer.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}
