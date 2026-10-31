use protocol::{ModelFailure, RetryPolicy};

/// One checked retry schedule produced from the current state and policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetrySchedule {
    pub(super) attempt: u32,
    pub(super) max_attempts: u32,
    pub(super) delay_ms: u64,
}

/// A logical generation borrows immutable policy while owning its single retry budget.
pub(super) struct RetryState<'a> {
    attempts: u32,
    policy: &'a RetryPolicy,
    provider_delay_limit_ms: u64,
}

impl<'a> RetryState<'a> {
    /// Starts a budget shared by request acquisition and response-stream failures.
    pub(super) fn new(
        policy: &'a RetryPolicy,
        provider_delay_limit_ms: u64,
    ) -> Self {
        Self {
            attempts: 0,
            policy,
            provider_delay_limit_ms,
        }
    }

    /// Reserves one retry, honoring server waits without nesting another retry loop.
    pub(super) fn schedule(
        &mut self,
        failure: &ModelFailure,
    ) -> Option<RetrySchedule> {
        if !failure.is_retryable()
            || !self.policy.enabled
            || self.attempts >= self.policy.max_retries
        {
            return None;
        }
        let attempt = self.attempts.checked_add(1)?;
        let delay_ms = match failure.retry_after_ms {
            Some(delay)
                if self.provider_delay_limit_ms > 0
                    && delay > self.provider_delay_limit_ms =>
            {
                tracing::warn!(
                    "Provider requested {} ms retry delay, exceeding {} ms limit",
                    delay,
                    self.provider_delay_limit_ms
                );
                return None;
            }
            Some(delay) => delay,
            None => {
                // Saturate before applying the cap; large configured budgets remain usable.
                let factor = 1_u64
                    .checked_shl(attempt.checked_sub(1)?)
                    .unwrap_or(u64::MAX);
                let delay = self.policy.base_delay_ms.saturating_mul(factor);
                if self.policy.max_retry_delay_ms > 0 {
                    delay.min(self.policy.max_retry_delay_ms)
                } else {
                    delay
                }
            }
        };
        self.attempts = attempt;
        Some(RetrySchedule {
            attempt,
            max_attempts: self.policy.max_retries,
            delay_ms,
        })
    }

    /// Resets the budget after a successful generation without replacing its policy context.
    pub(super) fn reset(&mut self) {
        self.attempts = 0;
    }

    /// Returns how many retries the current generation has consumed.
    pub(super) const fn attempts(&self) -> u32 {
        self.attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ModelRetryDisposition;

    /// The optional provider budget overrides rather than multiplies the runtime budget.
    #[test]
    fn one_budget_covers_every_provider_attempt() {
        for (enabled, override_, retries) in [
            (true, None, 5),
            (true, Some(2), 2),
            (true, Some(0), 0),
            (false, Some(2), 0),
        ] {
            let mut config = config::RetryConfig::default();
            config.agent.enabled = enabled;
            config.agent.max_retries = 5;
            config.provider.max_retries = override_;
            let policy = config.model_policy();
            let mut state = RetryState::new(&policy, 60000);
            let failure = ModelFailure::builder()
                .summary("rate limit".into())
                .retry_disposition(ModelRetryDisposition::Retryable)
                .build();
            assert_eq!(
                (0..8).filter_map(|_| state.schedule(&failure)).count(),
                retries
            );
        }
    }

    /// A server delay is honored whole or refused, never shortened to an unsafe retry.
    #[test]
    fn server_delay_is_not_clipped_to_the_local_backoff_cap() {
        let policy = RetryPolicy::builder()
            .enabled(true)
            .max_retries(2)
            .base_delay_ms(1)
            .max_retry_delay_ms(10)
            .build();
        let mut state = RetryState::new(&policy, 60000);
        let long = ModelFailure::builder()
            .summary("rate limit".into())
            .retry_disposition(ModelRetryDisposition::Retryable)
            .retry_after_ms(Some(120000))
            .build();
        assert!(state.schedule(&long).is_none());
        assert_eq!(state.attempts(), 0);
        let allowed = ModelFailure::builder()
            .summary("rate limit".into())
            .retry_disposition(ModelRetryDisposition::Retryable)
            .retry_after_ms(Some(500))
            .build();
        assert_eq!(state.schedule(&allowed).unwrap().delay_ms, 500);
    }
    /// A bounded or zero backoff remains valid after the exponent exceeds a machine word.
    #[test]
    fn capped_backoff_does_not_truncate_the_retry_budget() {
        for base in [0, 1] {
            let policy = RetryPolicy::builder()
                .enabled(true)
                .max_retries(70)
                .base_delay_ms(base)
                .max_retry_delay_ms(10)
                .build();
            let mut state = RetryState::new(&policy, 60000);
            let failure = ModelFailure::builder()
                .summary("rate limit".into())
                .retry_disposition(ModelRetryDisposition::Retryable)
                .build();
            for _ in 0..70 {
                assert!(state.schedule(&failure).is_some());
            }
            assert!(state.schedule(&failure).is_none());
        }
    }
}
