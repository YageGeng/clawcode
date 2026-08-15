use std::collections::BTreeMap;
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

    /// Starts one response with optional provider-boundary request hooks.
    async fn stream_with_hooks(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
        _hooks: Option<Arc<dyn provider::completion::CompletionRequestHooks>>,
    ) -> Result<ModelStream, ModelError> {
        self.stream(request, cancellation).await
    }
}

/// Factory interface used by kernel construction to acquire the active model.
pub trait ModelFactory: Send + Sync {
    /// Creates or resolves the model selected for this kernel lifecycle.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError>;

    /// Creates the immutable model catalog after static extensions are frozen.
    fn create_catalog(
        &self,
        _registration: &protocol::StaticExtensionRegistration,
    ) -> Result<ModelCatalog, ModelError> {
        Ok(ModelCatalog::single(self.create()?))
    }
}

/// Immutable provider/model lookup used by session-local model selection.
pub struct ModelCatalog {
    active_key: String,
    models: BTreeMap<String, Arc<dyn Model>>,
}

impl ModelCatalog {
    /// Creates a one-model catalog for factories without multi-model support.
    #[must_use]
    pub fn single(model: Arc<dyn Model>) -> Self {
        let profile = model.profile();
        let active_key =
            format!("{}/{}", profile.provider_id, profile.model_id);
        Self {
            active_key: active_key.clone(),
            models: BTreeMap::from([(active_key, model)]),
        }
    }

    /// Creates a validated catalog with one explicitly selected active model.
    pub fn new(
        active_provider_id: &str,
        active_model_id: &str,
        models: Vec<Arc<dyn Model>>,
    ) -> Result<Self, ModelError> {
        let active_key = format!("{active_provider_id}/{active_model_id}");
        let mut indexed = BTreeMap::new();
        for model in models {
            let profile = model.profile();
            let key = format!("{}/{}", profile.provider_id, profile.model_id);
            if indexed.insert(key.clone(), model).is_some() {
                return Err(ModelError::Unavailable(format!(
                    "model catalog contains duplicate {key}"
                )));
            }
        }
        if !indexed.contains_key(&active_key) {
            return Err(ModelError::Unavailable(format!(
                "active model is absent from catalog: {active_key}"
            )));
        }
        Ok(Self {
            active_key,
            models: indexed,
        })
    }

    /// Returns the model selected by immutable application configuration.
    #[must_use]
    pub fn active(&self) -> Arc<dyn Model> {
        Arc::clone(
            self.models
                .get(&self.active_key)
                .expect("validated model catalog contains active model"),
        )
    }

    /// Resolves one configured provider/model pair.
    #[must_use]
    pub fn resolve(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Option<Arc<dyn Model>> {
        self.models
            .get(&format!("{provider_id}/{model_id}"))
            .map(Arc::clone)
    }

    /// Returns model profiles in stable provider/model lexical order.
    #[must_use]
    pub fn profiles(&self) -> Vec<ModelProfile> {
        self.models
            .values()
            .map(|model| model.profile().clone())
            .collect()
    }
}
