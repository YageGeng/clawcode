//! Token usage snapshots shared by providers, persistence, and frontend replay.

use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// Current model context-window occupancy for an outgoing request.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct ContextWindowUsage {
    /// Estimated tokens currently sent as model context.
    pub used_tokens: u64,
    /// Total model context window size in tokens.
    pub context_tokens: u64,
}

impl ContextWindowUsage {
    /// Creates a context-window usage snapshot.
    pub fn new(used_tokens: u64, context_tokens: u64) -> Self {
        Self {
            used_tokens,
            context_tokens,
        }
    }
}

/// Provider-reported token usage for one model response.
/// If tokens used are `0`, then the provider failed to supply token usage metrics.
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
    /// The number of input ("prompt") tokens used in a given request.
    pub input_tokens: u64,
    /// The number of output ("completion") tokens used in a given request.
    pub output_tokens: u64,
    /// We store this separately as some providers may only report one number.
    pub total_tokens: u64,
    /// The number of input tokens read from a provider-managed cache.
    pub cached_input_tokens: u64,
    /// The number of input tokens written to a provider-managed cache.
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    /// Creates a new zero-valued usage snapshot.
    pub fn new() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    /// Return the default display total used by runtime and replay status updates.
    pub fn display_tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl Default for Usage {
    fn default() -> Self {
        Self::new()
    }
}

impl Add for Usage {
    type Output = Self;

    /// Add two provider-reported usage snapshots into a new accumulated total.
    fn add(self, other: Self) -> Self::Output {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
            cached_input_tokens: self.cached_input_tokens
                + other.cached_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens
                + other.cache_creation_input_tokens,
        }
    }
}

impl AddAssign for Usage {
    /// Add another provider-reported usage snapshot into this accumulated total.
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }
}
