use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use protocol::{
    DeviceCodeSink, LoginMethod, ModelFailure, ModelProfile, ModelRequest,
    ModelStreamEvent,
};
use tokio_util::sync::CancellationToken;

/// Boxed model stream shared by production adapters and deterministic tests.
pub type ModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Typed model construction and streaming failures.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
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
    /// The provider says this model cannot be used until the user signs in.
    #[error("sign-in required for provider {method_id}")]
    AuthRequired {
        /// Login method that resolves this failure.
        method_id: String,
    },
}

impl From<protocol::ScalarError> for ModelError {
    /// Keeps invalid provider identities at the existing protocol-error boundary.
    fn from(error: protocol::ScalarError) -> Self {
        Self::Protocol(error.to_string())
    }
}

/// Provider-neutral streaming model used by the kernel.
#[async_trait]
pub trait Model: Send + Sync {
    /// Returns the immutable provider/model profile used by this handle.
    fn profile(&self) -> &ModelProfile;

    /// Validates model readiness without sending a paid inference request.
    async fn preflight(&self) -> Result<(), ModelError>;

    /// Returns the method a client can use to sign in to this model's provider.
    fn login_method(&self) -> Option<LoginMethod> {
        None
    }

    /// Reports `AuthRequired` when this model cannot be used before a sign-in.
    async fn ensure_signed_in(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Runs one interactive sign-in, handing its device code to `on_code`.
    ///
    /// Dropping the returned future stops the sign-in.
    async fn sign_in(
        &self,
        _on_code: DeviceCodeSink,
    ) -> Result<(), ModelError> {
        Err(ModelError::Unavailable(format!(
            "model {}/{} has no sign-in",
            self.profile().provider_id,
            self.profile().model_id
        )))
    }

    /// Starts one streamed model response for the current Turn context.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError>;

    /// Starts one response with request-scoped native HTTP middleware.
    async fn stream_with_middleware(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
        _middleware: Option<extension::ProviderMiddleware>,
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

    /// Lists each sign-in once, however many models of a provider share it.
    #[must_use]
    pub fn login_methods(&self) -> Vec<LoginMethod> {
        let mut methods = BTreeMap::new();
        for method in self
            .models
            .values()
            .filter_map(|model| model.login_method())
        {
            methods.entry(method.id.clone()).or_insert(method);
        }
        methods.into_values().collect()
    }

    /// Runs one sign-in through any model that offers `method_id`.
    pub async fn sign_in(
        &self,
        method_id: &str,
        on_code: DeviceCodeSink,
    ) -> Result<(), ModelError> {
        let model = self
            .models
            .values()
            .find(|model| {
                model
                    .login_method()
                    .is_some_and(|method| method.id == method_id)
            })
            .ok_or_else(|| {
                ModelError::Unavailable(format!(
                    "no login method named {method_id}"
                ))
            })?;
        model.sign_in(on_code).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A model that records which profile was asked to sign in.
    struct LoginFake {
        profile: ModelProfile,
        method: Option<&'static str>,
        signed_in: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Model for LoginFake {
        /// Returns the scripted identity.
        fn profile(&self) -> &ModelProfile {
            &self.profile
        }

        /// Never blocks these catalog tests.
        async fn preflight(&self) -> Result<(), ModelError> {
            Ok(())
        }

        /// Returns the scripted method under the shared display name.
        fn login_method(&self) -> Option<LoginMethod> {
            self.method.map(|id| LoginMethod {
                id: id.to_owned(),
                name: id.to_uppercase(),
            })
        }

        /// Records the model that received the sign-in.
        async fn sign_in(
            &self,
            _on_code: DeviceCodeSink,
        ) -> Result<(), ModelError> {
            self.signed_in
                .lock()
                .expect("sign-ins")
                .push(self.profile.provider_id.clone());
            Ok(())
        }

        /// These catalog tests never stream.
        async fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> Result<ModelStream, ModelError> {
            Err(ModelError::Unavailable("not streamed".to_owned()))
        }
    }

    /// Builds a catalog from `(provider, model, method)` rows.
    fn catalog(
        rows: &[(&str, &str, Option<&'static str>)],
        signed_in: &Arc<Mutex<Vec<String>>>,
    ) -> ModelCatalog {
        let models = rows
            .iter()
            .map(|(provider, model, method)| {
                Arc::new(LoginFake {
                    profile: ModelProfile::builder()
                        .provider_id((*provider).to_owned())
                        .model_id((*model).to_owned())
                        .display_name((*model).to_owned())
                        .context_tokens(1_000)
                        .max_output_tokens(100)
                        .build(),
                    method: *method,
                    signed_in: Arc::clone(signed_in),
                }) as Arc<dyn Model>
            })
            .collect();
        ModelCatalog::new(rows[0].0, rows[0].1, models).expect("catalog")
    }

    /// One provider's models share one method; keyed providers list none.
    #[test]
    fn login_methods_are_deduplicated() {
        let methods = catalog(
            &[
                ("chatgpt", "a", Some("chatgpt")),
                ("chatgpt", "b", Some("chatgpt")),
                ("corp", "c", None),
            ],
            &Arc::default(),
        )
        .login_methods();
        assert_eq!(
            methods,
            [LoginMethod {
                id: "chatgpt".to_owned(),
                name: "CHATGPT".to_owned(),
            }]
        );
    }

    /// A sign-in reaches the provider that offers the method, and only that one.
    #[tokio::test]
    async fn catalog_sign_in_forwards_to_the_matching_model() {
        let signed_in = Arc::default();
        let catalog = catalog(
            &[("corp", "c", None), ("chatgpt", "a", Some("chatgpt"))],
            &signed_in,
        );
        catalog
            .sign_in("chatgpt", Arc::new(|_code| {}))
            .await
            .expect("signed in");
        assert_eq!(*signed_in.lock().expect("sign-ins"), ["chatgpt"]);
        assert!(matches!(
            catalog.sign_in("missing", Arc::new(|_code| {})).await,
            Err(ModelError::Unavailable(_))
        ));
    }
}
