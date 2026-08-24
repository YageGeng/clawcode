//! Immutable retry settings aligned with pi v4 runtime behavior.

use protocol::RetryPolicy;
use serde::{Deserialize, Serialize};

/// Request-level retry controls applied at the provider boundary.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderRetryConfig {
    /// Optional provider request timeout in milliseconds; zero disables it.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Optional number of request retries beyond the initial provider request.
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// Maximum provider-requested delay; zero disables the delay cap.
    #[serde(default = "ProviderRetryConfig::default_max_retry_delay_ms")]
    pub max_retry_delay_ms: u64,
}

impl ProviderRetryConfig {
    /// Returns pi's default cap for a server-requested retry delay.
    const fn default_max_retry_delay_ms() -> u64 {
        60_000
    }
}

impl Default for ProviderRetryConfig {
    /// Returns provider retry settings that preserve provider-specific retry defaults.
    fn default() -> Self {
        Self {
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: Self::default_max_retry_delay_ms(),
        }
    }
}

/// Assistant response retry policy with bounded exponential backoff.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RetryConfig {
    /// Provider-neutral assistant retry policy.
    #[serde(flatten)]
    pub agent: RetryPolicy,
    /// Provider request-level retry controls.
    #[serde(default)]
    pub provider: ProviderRetryConfig,
}

impl RetryConfig {
    /// Checks whether the configured exponential backoff fits in milliseconds.
    pub(crate) fn has_valid_backoff(&self) -> bool {
        if !self.agent.enabled
            || self.agent.max_retries == 0
            || self.agent.base_delay_ms == 0
        {
            return true;
        }

        // A positive cap bounds every scheduled delay to a safe finite value.
        if self.agent.max_retry_delay_ms > 0 {
            return true;
        }

        // Without a cap the multiplicative backoff must still fit in milliseconds.
        1_u64
            .checked_shl(self.agent.max_retries - 1)
            .and_then(|factor| self.agent.base_delay_ms.checked_mul(factor))
            .is_some()
    }
}

impl Default for RetryConfig {
    /// Returns pi's assistant and provider retry defaults.
    fn default() -> Self {
        Self {
            agent: RetryPolicy::default(),
            provider: ProviderRetryConfig::default(),
        }
    }
}
