use protocol::{ModelFailure, ModelRetryDisposition, RetryPolicy};

/// Pi-compatible classifier for failed Assistant messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetryClassifier;

impl RetryClassifier {
    /// Classifies structured failures using pi's provider-error vocabulary.
    #[must_use]
    pub fn classify(failure: &ModelFailure) -> ModelRetryDisposition {
        let message = failure.summary.to_ascii_lowercase();
        let non_retryable = [
            "gousagelimiterror",
            "freeusagelimiterror",
            "monthly usage limit reached",
            "available balance",
            "insufficient_quota",
            "out of budget",
            "quota exceeded",
            "billing",
            "authentication",
            "unauthorized",
            "invalid api key",
            "context window",
            "context length",
            "too many tokens",
        ]
        .iter()
        .any(|pattern| message.contains(pattern));
        if non_retryable
            || failure.retry_disposition == ModelRetryDisposition::NonRetryable
        {
            return ModelRetryDisposition::NonRetryable;
        }

        let retryable = [
            "overloaded",
            "rate limit",
            "rate-limit",
            "too many requests",
            "service unavailable",
            "server error",
            "internal error",
            "provider returned error",
            "request buffer limit",
            "network error",
            "connection error",
            "connection refused",
            "connection lost",
            "other side closed",
            "fetch failed",
            "getaddrinfo",
            "enotfound",
            "eai_again",
            "upstream connect",
            "reset before headers",
            "socket hang up",
            "socket connection was closed",
            "timed out",
            "timeout",
            "terminated",
            "websocket closed",
            "websocket error",
            "ended without",
            "stream ended before message_stop",
            "stream ended before a terminal response event",
            "http2 request did not get a response",
            "retry delay",
            "you can retry your request",
            "try your request again",
            "please retry your request",
            "resourceexhausted",
        ]
        .iter()
        .any(|pattern| message.contains(pattern));
        let retryable_status =
            matches!(failure.status, Some(429 | 500 | 502 | 503 | 504));
        if retryable || retryable_status {
            ModelRetryDisposition::Retryable
        } else {
            ModelRetryDisposition::NonRetryable
        }
    }
}

/// One checked retry schedule produced from the current state and policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetrySchedule {
    /// One-based retry number.
    pub(super) attempt: u32,
    /// Maximum retries permitted by policy.
    pub(super) max_attempts: u32,
    /// Exponential backoff delay before this retry.
    pub(super) delay_ms: u64,
}

/// Mutable retry budget retained across Turns in one run.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RetryState {
    attempts: u32,
}

impl RetryState {
    /// Reserves the next retry and computes a checked exponential delay.
    pub(super) fn schedule(
        &mut self,
        policy: &RetryPolicy,
    ) -> Option<RetrySchedule> {
        if !policy.enabled || self.attempts >= policy.max_retries {
            return None;
        }
        let attempt = self.attempts.checked_add(1)?;
        let factor = 1_u64.checked_shl(attempt.checked_sub(1)?)?;
        // Saturating multiplication keeps the delay bounded for large user config;
        // the explicit cap then enforces a hard ceiling on the backoff sleep.
        let mut delay_ms = policy.base_delay_ms.saturating_mul(factor);
        if policy.max_retry_delay_ms > 0 {
            delay_ms = delay_ms.min(policy.max_retry_delay_ms);
        }
        self.attempts = attempt;
        Some(RetrySchedule {
            attempt,
            max_attempts: policy.max_retries,
            delay_ms,
        })
    }

    /// Returns how many retries have been scheduled in the current run.
    pub(super) const fn attempts(&self) -> u32 {
        self.attempts
    }
}
