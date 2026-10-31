//! Immutable configuration definitions shared by independent session model bindings.

use std::{collections::HashMap, sync::Arc, time::Duration};

use config::{
    LlmModel, LlmProvider, ProviderId, ProviderRetryConfig, ProviderType,
};
use rig_core::providers::chatgpt::auth::{
    AuthContext, AuthError, Authenticator,
};
use rig_core::{
    DynModel, Model,
    completion::CompletionRequest,
    http_client::DynHttpClient,
    operation::Completion,
    providers::{anthropic, chatgpt, openai},
};
use serde_json::Value;

/// Configuration failures retained for a provider/model lookup without leaking credentials.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderBuildError {
    /// A caller requested a model absent from the explicit configuration.
    #[error("provider model '{0}' is not configured")]
    NotConfigured(String),
    /// A configured entry failed validation or client construction.
    #[error("provider model '{model}' could not be built: {reason}")]
    Invalid { model: String, reason: String },
}

/// Authentication and upstream request failures stay distinguishable at the model boundary.
#[derive(Debug)]
pub enum ProviderConnectError {
    /// The upstream authenticator could not supply a credential.
    Authentication(Box<AuthError>),
    /// A subscription model was constructed without its resolved credential.
    MissingAuthentication,
}

impl std::fmt::Display for ProviderConnectError {
    /// Emits actionable typed categories without exposing upstream auth text or credentials.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Authentication(error) = self else {
            return formatter.write_str(
                "missing authentication context for model construction",
            );
        };
        formatter.write_str("ChatGPT authentication failed: ")?;
        // AuthError Display may contain response bodies, URLs, file contents, or store secrets.
        match error.as_ref() {
            AuthError::Io(error) => {
                write!(formatter, "I/O ({:?})", error.kind())
            }
            AuthError::Json(error) => write!(
                formatter,
                "JSON {:?} at line {}, column {}",
                error.classify(),
                error.line(),
                error.column()
            ),
            AuthError::Http(error) => match error.non_success_status() {
                Some(status) => write!(formatter, "HTTP {}", status.as_u16()),
                None => formatter.write_str("HTTP transport or protocol error"),
            },
            AuthError::Store(_) => formatter.write_str("token store failure"),
            AuthError::Message(_) => {
                formatter.write_str("authentication flow failure")
            }
        }
    }
}

impl std::error::Error for ProviderConnectError {
    /// Retains the original source for typed inspection, never for ordinary diagnostic logging.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Authentication(error) => Some(error.as_ref()),
            Self::MissingAuthentication => None,
        }
    }
}

/// A boxed upstream model keeps large wire configurations off request futures.
/// Cloning preserves its session-owned Prepared history.
#[derive(Clone, Debug)]
pub enum ProviderCompletion {
    /// All OpenAI-family dialects select Chat or Responses through their wire.
    Openai(Box<Model<openai::wire::OpenAiWire>>),
    /// Anthropic and its compatible dialects share Messages.
    Anthropic(Box<Model<anthropic::wire::Messages>>),
}

impl ProviderCompletion {
    /// Attaches a request-local transport without rebuilding the cloned upstream model.
    pub fn with_transport(self, http: DynHttpClient) -> DynModel<Completion> {
        match self {
            Self::Openai(mut model) => {
                model.transport = http;
                (*model).erase()
            }
            Self::Anthropic(mut model) => {
                model.transport = http;
                (*model).erase()
            }
        }
    }
}

/// The upstream configuration family used to construct an independent session model.
#[derive(Clone)]
enum Definition {
    Openai(Box<openai::OpenAIConfig>),
    Anthropic(Box<anthropic::AnthropicConfig>),
}

/// A shared definition owns no conversation history or request middleware.
#[derive(typed_builder::TypedBuilder)]
pub struct ConfiguredModel {
    profile: protocol::ModelProfile,
    upstream_id: String,
    definition: Definition,
    parameters: Value,
    http: DynHttpClient,
    #[builder(default)]
    auth: Option<Authenticator>,
    #[builder(default)]
    timeout_ms: Option<u64>,
}

impl std::fmt::Debug for ConfiguredModel {
    /// Prints only public model identity, never keys, parameters, or auth state.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfiguredModel")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl ConfiguredModel {
    /// Validates one explicit model and binds the upstream dialect without sending a request.
    fn new(
        provider: &LlmProvider,
        model: &LlmModel,
        retry: &ProviderRetryConfig,
        http: DynHttpClient,
    ) -> Result<Self, String> {
        provider
            .validate_auth_source()
            .map_err(|error| error.to_string())?;
        let profile = model
            .to_profile(provider)
            .map_err(|error| error.to_string())?;
        let key = provider
            .api_key
            .as_ref()
            .map(config::ApiKeyConfig::resolve)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let definition = match provider.provider_type {
            ProviderType::Responses | ProviderType::OpenaiCompletions => {
                let dialect = match (&provider.id, provider.provider_type) {
                    (ProviderId::Openai | ProviderId::Other(_), _) => {
                        &openai::wire::OPENAI
                    }
                    (ProviderId::Chatgpt, ProviderType::Responses) => {
                        &chatgpt::DIALECT
                    }
                    (ProviderId::Deepseek, ProviderType::OpenaiCompletions) => {
                        &openai::wire::DEEPSEEK
                    }
                    (ProviderId::Moonshot, ProviderType::OpenaiCompletions) => {
                        &openai::wire::MOONSHOT
                    }
                    (ProviderId::Minimax, ProviderType::OpenaiCompletions) => {
                        &openai::wire::MINIMAX
                    }
                    (
                        ProviderId::Xiaomimimo,
                        ProviderType::OpenaiCompletions,
                    ) => &openai::wire::XIAOMIMIMO,
                    _ => {
                        return Err(
                            "provider does not support the configured protocol"
                                .to_owned(),
                        );
                    }
                };
                let route = if provider.provider_type == ProviderType::Responses
                {
                    openai::wire::Route::Responses
                } else {
                    openai::wire::Route::Chat
                };
                Definition::Openai(Box::new(
                    openai::OpenAIConfig::with_key(dialect, key)
                        .with_base_url(&provider.base_url)
                        .with_route(route),
                ))
            }
            ProviderType::Anthropic => {
                let dialect =
                    match &provider.id {
                        ProviderId::Anthropic | ProviderId::Other(_) => {
                            &anthropic::wire::ANTHROPIC
                        }
                        ProviderId::Moonshot => &anthropic::wire::MOONSHOT,
                        ProviderId::Minimax => &anthropic::wire::MINIMAX,
                        ProviderId::Xiaomimimo => &anthropic::wire::XIAOMIMIMO,
                        _ => return Err(
                            "provider does not support the configured protocol"
                                .to_owned(),
                        ),
                    };
                Definition::Anthropic(Box::new(
                    anthropic::AnthropicConfig::with_key(dialect, key)
                        .with_base_url(&provider.base_url),
                ))
            }
        };
        Ok(Self::builder()
            .profile(profile)
            .upstream_id(
                model
                    .upstream_id
                    .clone()
                    .unwrap_or_else(|| model.id.clone()),
            )
            .definition(definition)
            .parameters(model.extra_param.clone())
            .http(http)
            .auth(provider.auth.as_ref().map(Self::authenticator))
            .timeout_ms(retry.timeout_ms)
            .build())
    }

    /// Returns the immutable project identity and upstream-compatible input capabilities.
    pub fn profile(&self) -> &protocol::ModelProfile {
        &self.profile
    }

    /// Clones the shared connection pool without attaching request state.
    pub fn http_client(&self) -> DynHttpClient {
        self.http.clone()
    }

    /// Resolves the shared upstream authenticator before any session cache lock is taken.
    pub async fn auth_context(
        &self,
    ) -> Result<Option<AuthContext>, ProviderConnectError> {
        match &self.auth {
            Some(auth) => Ok(Some(
                auth.auth_context(&self.http).await.map_err(|error| {
                    ProviderConnectError::Authentication(Box::new(error))
                })?,
            )),
            None => Ok(None),
        }
    }

    /// Builds fresh Prepared state for a new session or a changed credential.
    pub fn build_model(
        &self,
        auth: Option<&AuthContext>,
    ) -> Result<ProviderCompletion, ProviderConnectError> {
        if self.auth.is_some() && auth.is_none() {
            return Err(ProviderConnectError::MissingAuthentication);
        }
        let timeout = self
            .timeout_ms
            .map(|ms| (ms > 0).then(|| Duration::from_millis(ms)));
        Ok(match self.definition.clone() {
            Definition::Openai(mut config) => {
                if let Some(auth) = auth {
                    config.api_key = auth.api_key.clone();
                    config.account_id.clone_from(&auth.account_id);
                    if let Some(base) = &auth.api_base {
                        config.base_url.clone_from(base);
                    }
                }
                let model = config
                    .connect(self.http.clone())
                    .completion(&self.upstream_id);
                ProviderCompletion::Openai(Box::new(match timeout {
                    Some(timeout) => model.with_idle_timeout(timeout),
                    None => model,
                }))
            }
            Definition::Anthropic(config) => {
                let model = config
                    .connect(self.http.clone())
                    .completion(&self.upstream_id);
                ProviderCompletion::Anthropic(Box::new(match timeout {
                    Some(timeout) => model.with_idle_timeout(timeout),
                    None => model,
                }))
            }
        })
    }

    /// Preserves nested project defaults; upstream additional_params merges only top-level keys.
    pub fn prepare(&self, mut request: CompletionRequest) -> CompletionRequest {
        // Explicit models must work even when upstream has no catalog output limit.
        request
            .max_tokens
            .get_or_insert(self.profile.max_output_tokens);
        request.additional_params = match request.additional_params.take() {
            Some(overrides) => {
                let mut parameters = self.parameters.clone();
                Self::overlay(&mut parameters, overrides);
                Some(parameters)
            }
            None => {
                (!self.parameters.is_null()).then(|| self.parameters.clone())
            }
        };
        request
    }

    /// Overlays request keys recursively without changing provider wire conversion.
    fn overlay(defaults: &mut Value, overrides: Value) {
        match (defaults, overrides) {
            (Value::Object(defaults), Value::Object(overrides)) => {
                for (key, value) in overrides {
                    Self::overlay(
                        defaults.entry(key).or_insert(Value::Null),
                        value,
                    );
                }
            }
            (defaults, overrides) => *defaults = overrides,
        }
    }
}

/// Shared provider definitions contain only configuration, authentication, and transport.
pub struct ProviderFactory {
    models: HashMap<String, Arc<ConfiguredModel>>,
    failures: HashMap<String, String>,
}

impl ProviderFactory {
    /// Builds all explicit definitions once, retaining safe failures for strict lookups.
    pub fn new(config: config::ConfigHandle) -> Self {
        let config = config.current();
        let http = rig_reqwest::shared();
        let mut models = HashMap::new();
        let mut failures = HashMap::new();
        for provider in &config.providers {
            for model in &provider.models {
                let key = format!("{}/{}", provider.id.as_str(), model.id);
                if models.contains_key(&key) || failures.contains_key(&key) {
                    continue;
                }
                match ConfiguredModel::new(
                    provider,
                    model,
                    &config.retry.provider,
                    http.clone(),
                ) {
                    Ok(model) => {
                        models.insert(key, Arc::new(model));
                    }
                    Err(reason) => {
                        failures.insert(key, reason);
                    }
                }
            }
        }
        Self { models, failures }
    }

    /// Resolves a shared definition without creating a global model-history cache.
    pub fn resolve(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Arc<ConfiguredModel>, ProviderBuildError> {
        let key = format!("{provider}/{model}");
        if let Some(model) = self.models.get(&key) {
            return Ok(Arc::clone(model));
        }
        if let Some(reason) = self.failures.get(&key) {
            return Err(ProviderBuildError::Invalid {
                model: key,
                reason: reason.clone(),
            });
        }
        Err(ProviderBuildError::NotConfigured(key))
    }
}
