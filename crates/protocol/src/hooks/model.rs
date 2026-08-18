//! Provider-boundary request hooks shared by the kernel and provider adapters.
//!
//! The kernel's `Model` trait accepts these hooks so the runtime can compose
//! extension lifecycle points without depending on any provider-specific type.
//! The provider crate implements the transport half of the contract while the
//! kernel implements the extension half.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

/// Normalized non-secret headers visible at the provider request boundary.
///
/// Credentials and cookies are projected away before a hook ever observes the
/// map, so a hostile hook cannot read or replace immutable provider data.
pub type ModelHeaders = BTreeMap<String, String>;

/// Sanitized provider response metadata observed before stream consumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResponseMetadata {
    /// HTTP or WebSocket handshake status.
    pub status: u16,
    /// Normalized response headers with sensitive values removed.
    pub headers: ModelHeaders,
}

/// Failure reported by one request-bound model hook implementation.
#[derive(Debug, thiserror::Error)]
pub enum ModelHookError {
    /// The hook rejected or could not transform the provider payload.
    #[error("model hook rejected the provider payload: {0}")]
    Payload(String),
    /// The hook rejected or could not transform visible provider headers.
    #[error("model hook rejected provider headers: {0}")]
    Headers(String),
    /// The hook failed to observe sanitized provider response metadata.
    #[error("model hook failed to observe the provider response: {0}")]
    Response(String),
}

/// Request-local hook contract applied at the provider boundary.
///
/// The three phases mirror pi's extension lifecycle: payload replacement,
/// header mutation, and sanitized response observation. Implementations must
/// be cheap and side-effect free beyond the provider request they bound to.
#[async_trait]
pub trait ModelRequestHooks: Send + Sync {
    /// Replaces one provider-native JSON payload after serialization.
    async fn before_payload(
        &self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, ModelHookError>;

    /// Replaces visible non-secret headers immediately before transport.
    async fn before_headers(
        &self,
        headers: ModelHeaders,
    ) -> Result<ModelHeaders, ModelHookError>;

    /// Observes sanitized response metadata before body or stream consumption.
    async fn after_response(
        &self,
        response: ModelResponseMetadata,
    ) -> Result<(), ModelHookError>;
}

/// Convenience alias for boxed hook handles passed through request plumbing.
pub type ModelRequestHooksRef = Arc<dyn ModelRequestHooks>;
