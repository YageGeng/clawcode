use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// Provider-reported token usage for one model response.
#[derive(
    Debug,
    PartialEq,
    Eq,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct Usage {
    /// Number of input tokens consumed by the request.
    pub input_tokens: u64,

    /// Number of output tokens produced by the response.
    pub output_tokens: u64,

    /// Provider-reported total token count.
    pub total_tokens: u64,

    /// Input tokens read from a provider-managed cache.
    pub cached_input_tokens: u64,

    /// Input tokens written to a provider-managed cache.
    pub cache_creation_input_tokens: u64,

    /// Reasoning tokens reported separately by providers that expose them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    /// Creates a zero-valued token usage snapshot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_tokens: None,
        }
    }

    /// Returns the input-plus-output total used for display.
    #[must_use]
    pub const fn display_tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl Default for Usage {
    /// Returns a zero-valued token usage snapshot.
    fn default() -> Self {
        Self::new()
    }
}

impl Add for Usage {
    type Output = Self;

    /// Adds two provider token usage snapshots.
    fn add(self, other: Self) -> Self::Output {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
            cached_input_tokens: self.cached_input_tokens
                + other.cached_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens
                + other.cache_creation_input_tokens,
            reasoning_tokens: match (
                self.reasoning_tokens,
                other.reasoning_tokens,
            ) {
                (Some(left), Some(right)) => Some(left + right),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            },
        }
    }
}

impl AddAssign for Usage {
    /// Adds another provider token usage snapshot in place.
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.reasoning_tokens =
            match (self.reasoning_tokens, other.reasoning_tokens) {
                (Some(left), Some(right)) => Some(left + right),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            };
    }
}
