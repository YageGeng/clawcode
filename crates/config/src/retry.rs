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
    /// Combines the historical request override with the one runtime retry budget.
    #[must_use]
    pub fn model_policy(&self) -> RetryPolicy {
        let mut policy = self.agent;
        if let Some(max_retries) = self.provider.max_retries {
            policy.max_retries = max_retries;
        }
        policy
    }

    /// Checks whether the configured exponential backoff fits in milliseconds.
    pub(crate) fn has_valid_backoff(&self) -> bool {
        // Provider overrides change the effective budget, including validation.
        let policy = self.model_policy();
        if !policy.enabled
            || policy.max_retries == 0
            || policy.base_delay_ms == 0
        {
            return true;
        }

        // A positive cap bounds every scheduled delay to a safe finite value.
        if policy.max_retry_delay_ms > 0 {
            return true;
        }

        // Without a cap the multiplicative backoff must still fit in milliseconds.
        1_u64
            .checked_shl(policy.max_retries - 1)
            .and_then(|factor| policy.base_delay_ms.checked_mul(factor))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Validation must use the same override budget that the Kernel will consume.
    #[test]
    fn backoff_validation_uses_the_effective_retry_budget() {
        let mut config = RetryConfig::default();
        config.agent.max_retry_delay_ms = 0;
        config.provider.max_retries = Some(100);
        assert!(!config.has_valid_backoff());
        config.agent.max_retries = 100;
        config.provider.max_retries = Some(0);
        assert!(config.has_valid_backoff());
    }
}
