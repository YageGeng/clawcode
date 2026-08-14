use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use protocol::{ModelFailure, ModelProfile, ModelRequest, ModelStreamEvent};
use tokio_util::sync::CancellationToken;

/// Boxed model stream shared by production adapters and deterministic tests.
pub type ModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Typed model construction and streaming failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// The selected model was not configured or could not be constructed.
    #[error("model unavailable: {0}")]
    Unavailable(String),
    /// Provider readiness checks failed before a request was sent.
    #[error("model preflight failed: {0:?}")]
    Preflight(ModelFailure),
    /// A provider request could not acquire its response stream.
    #[error("model request failed: {0:?}")]
    Request(ModelFailure),
    /// A provider response stream failed after acquisition.
    #[error("model stream failed: {0:?}")]
    Stream(ModelFailure),
    /// Provider data could not be represented in the runtime protocol.
    #[error("model protocol conversion failed: {0}")]
    Protocol(String),
    /// The model request was cancelled before a terminal response.
    #[error("model request cancelled")]
    Cancelled,
}

/// Provider-neutral streaming model used by the kernel.
#[async_trait]
pub trait Model: Send + Sync {
    /// Returns the immutable provider/model profile used by this handle.
    fn profile(&self) -> &ModelProfile;

    /// Validates model readiness without sending a paid inference request.
    async fn preflight(&self) -> Result<(), ModelError>;

    /// Starts one streamed model response for the current Turn context.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError>;
}

/// Factory interface used by kernel construction to acquire the active model.
pub trait ModelFactory: Send + Sync {
    /// Creates or resolves the model selected for this kernel lifecycle.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError>;
}
