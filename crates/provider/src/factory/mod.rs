//! Immutable configuration definitions shared by independent session model bindings.

use std::{collections::HashMap, sync::Arc, time::Duration};

use config::{LlmModel, LlmProvider, ProviderType};
use protocol::{ModelInputModalities, ModelInputModality, ModelProfile};
use rig_core::providers::chatgpt::auth::{
    AuthContext, AuthError, Authenticator,
};
use rig_core::{
    DynModel,
    catalog::{Catalog, ModelSpec, ReasoningSupport},
    completion::{
        CompletionRequest,
        options::{Effort, Reasoning},
    },
    http_client::DynHttpClient,
    operation::Completion,
    providers::{
        anthropic, gemini, openai,
        registry::{Format, ProviderConfig, ProviderId as RigProviderId},
    },
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

/// Authentication failures stay distinguishable at the model boundary.
pub enum ProviderConnectError {
    /// The upstream authenticator could not supply a credential.
    Authentication(Box<AuthError>),
}

impl std::fmt::Display for ProviderConnectError {
    /// Emits actionable typed categories without exposing upstream auth text or credentials.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Authentication(error) = self;
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

impl std::fmt::Debug for ProviderConnectError {
    /// Reuses the redacted Display so `{:?}` cannot print upstream bodies or tokens.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ProviderConnectError({self})")
    }
}

impl std::error::Error for ProviderConnectError {
    /// Retains the original source for typed inspection, never for ordinary diagnostic logging.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let Self::Authentication(error) = self;
        Some(error.as_ref())
    }
}

/// A shared definition owns no conversation history or request middleware.
#[derive(typed_builder::TypedBuilder)]
pub struct ConfiguredModel {
    profile: protocol::ModelProfile,
    upstream_id: String,
    /// Upstream preset with key, base URL and route already applied.
    config: ProviderConfig,
    parameters: Value,
    http: DynHttpClient,
    #[builder(default)]
    auth: Option<Authenticator>,
    /// Stream idle timeout resolved once from configuration; `None` waits indefinitely.
    #[builder(default)]
    idle_timeout: Option<Duration>,
    /// Effort tiers the catalog lists for the model, lowest first; empty when unknown.
    #[builder(default)]
    efforts: Vec<Effort>,
}

/// rig's effort tiers, lowest first, so levels compare by position.
const EFFORT_ORDER: [Effort; 6] = [
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::XHigh,
    Effort::Max,
];

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
    /// `auth` is the provider's shared authenticator, never one built per model; `app`
    /// supplies the stream timeout and the compaction reserve that bounds the request budget.
    fn new(
        provider: &LlmProvider,
        model: &LlmModel,
        app: &config::AppConfig,
        http: DynHttpClient,
        auth: Option<Authenticator>,
    ) -> Result<Self, String> {
        provider
            .validate_names()
            .map_err(|error| error.to_string())?;
        provider
            .validate_auth_source()
            .map_err(|error| error.to_string())?;
        model
            .validate(provider)
            .map_err(|error| error.to_string())?;
        let key = provider
            .api_key
            .as_ref()
            .map(config::ApiKeyConfig::resolve)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let upstream_id = model
            .upstream_id
            .clone()
            .unwrap_or_else(|| model.id.clone());
        let config = Self::provider_config(provider, key)?;
        let spec = RigProviderId::catalog(provider.preset())
            .and_then(|preset| Catalog::builtin().get(preset, &upstream_id))
            .map(|resolved| resolved.spec);
        let efforts = match spec.map(|spec| &spec.reasoning) {
            Some(ReasoningSupport::Listed { levels, .. }) => levels.clone(),
            _ => Vec::new(),
        };
        let profile = Self::profile_for(
            provider,
            model,
            spec,
            &format!("{}/{upstream_id}", provider.preset()),
            app.compaction.reserve_tokens,
        )?;
        Ok(Self::builder()
            .profile(profile)
            .upstream_id(upstream_id)
            .config(config)
            .parameters(model.extra_param.clone())
            .http(http)
            .auth(auth)
            // An omitted timeout keeps rig's default; zero disables it.
            .efforts(efforts)
            .idle_timeout(match app.retry.provider.timeout_ms {
                None => Some(rig_core::driver::STREAM_IDLE_TIMEOUT),
                Some(0) => None,
                Some(ms) => Some(Duration::from_millis(ms)),
            })
            .build())
    }

    /// Turns one provider entry into a rig preset, or a generic dialect for its protocol.
    ///
    /// The preset/protocol pair selects a registered rig preset; without one the
    /// entry must name its endpoint, and unknown presets keep the old Responses default.
    fn provider_config(
        provider: &LlmProvider,
        key: String,
    ) -> Result<ProviderConfig, String> {
        // rig calls this name a vendor; the configuration calls it a preset.
        let preset = provider.preset();
        let format = provider.provider_type.map(|protocol| match protocol {
            ProviderType::Responses | ProviderType::OpenaiCompletions => {
                Format::OpenAi
            }
            ProviderType::Anthropic => Format::Anthropic,
            ProviderType::Gemini => Format::Gemini,
        });
        let registered = match format {
            Some(format) => RigProviderId::new(preset, format),
            // `resolve` rejects vendors registered in two families; `catalog` picks the primary.
            None => RigProviderId::catalog(preset),
        }
        .and_then(|id| id.config(key.clone()));
        let (config, protocol) = match (
            registered,
            provider.base_url.as_deref(),
        ) {
            (Some(config), base_url) => (
                match base_url {
                    Some(url) => config.with_base_url(url),
                    None => config,
                },
                provider.provider_type,
            ),
            (None, Some(base_url)) => {
                // Unknown presets keep the old Responses default.
                let protocol =
                    provider.provider_type.unwrap_or(ProviderType::Responses);
                // A preset rig knows loses its dialect details here; a custom gateway
                // name is expected to use the generic dialect, so only the former is logged.
                if RigProviderId::catalog(preset).is_some() {
                    tracing::warn!(
                        "provider '{}' uses the generic dialect for protocol '{}': rig has no '{}' preset for it",
                        provider.id.as_str(),
                        protocol.as_str(),
                        preset
                    );
                }
                let config = match format.unwrap_or(Format::OpenAi) {
                    Format::OpenAi => ProviderConfig::OpenAi(
                        openai::OpenAIConfig::with_key(
                            &openai::wire::OPENAI,
                            key,
                        )
                        .with_base_url(base_url),
                    ),
                    Format::Anthropic => ProviderConfig::Anthropic(
                        anthropic::AnthropicConfig::with_key(
                            &anthropic::wire::ANTHROPIC,
                            key,
                        )
                        .with_base_url(base_url),
                    ),
                    Format::Gemini => ProviderConfig::Gemini(
                        gemini::GeminiConfig::new(key).with_base_url(base_url),
                    ),
                };
                (config, Some(protocol))
            }
            (None, None) => {
                return Err(format!(
                    "provider '{}' needs base_url: rig has no '{preset}' preset for protocol '{}'",
                    provider.id.as_str(),
                    provider
                        .provider_type
                        .map_or("responses (default)", ProviderType::as_str)
                ));
            }
        };
        // An explicit OpenAI protocol picks the route; otherwise the dialect and catalog decide.
        Ok(match (config, protocol) {
            (ProviderConfig::OpenAi(config), Some(ProviderType::Responses)) => {
                ProviderConfig::OpenAi(
                    config.with_route(openai::wire::Route::Responses),
                )
            }
            (
                ProviderConfig::OpenAi(config),
                Some(ProviderType::OpenaiCompletions),
            ) => ProviderConfig::OpenAi(
                config.with_route(openai::wire::Route::Chat),
            ),
            (config, _) => config,
        })
    }

    /// Merges configured model metadata over the rig catalog entry, configuration first.
    /// The request budget defaults to the output capability bounded by `reserve_tokens`;
    /// `catalog_key` is the `preset/upstream_id` looked up, named in missing-limit errors.
    fn profile_for(
        provider: &LlmProvider,
        model: &LlmModel,
        spec: Option<&ModelSpec>,
        catalog_key: &str,
        reserve_tokens: u64,
    ) -> Result<ModelProfile, String> {
        let limit = |field: &str,
                     configured: Option<u64>,
                     catalog: Option<u32>| {
            configured.or(catalog.map(u64::from)).ok_or_else(|| {
                format!(
                    "{field} is not configured and the rig catalog has no value for '{catalog_key}'"
                )
            })
        };
        // A provider input cap below the window bounds what one request may send.
        let context_tokens = limit(
            "context_tokens",
            model.context_tokens,
            spec.and_then(ModelSpec::input_limit),
        )?;
        let max_output_tokens = limit(
            "max_output_tokens",
            model.max_output_tokens,
            spec.and_then(|spec| spec.max_output_tokens),
        )?;
        // Compaction keeps the reserve free, so a budget within it fits the window.
        let request_max_tokens = match model.request_max_tokens {
            Some(budget) if budget > max_output_tokens => {
                return Err(format!(
                    "request_max_tokens={budget} exceeds max_output_tokens={max_output_tokens}"
                ));
            }
            Some(budget) => budget,
            None => max_output_tokens.min(reserve_tokens),
        };
        if request_max_tokens == 0 {
            return Err(
                "request_max_tokens resolves to 0; set request_max_tokens or compaction.reserve_tokens"
                    .to_owned(),
            );
        }
        let input = match &model.input {
            Some(input) => ModelInputModalities::try_from(input.clone()),
            None if spec.is_some_and(|spec| spec.input.image) => {
                ModelInputModalities::try_from(vec![
                    ModelInputModality::Text,
                    ModelInputModality::Image,
                ])
            }
            None => Ok(ModelInputModalities::default()),
        }
        .map_err(|error| error.to_string())?;
        // A tier the catalog says the model cannot honour would be skipped with a
        // warning on every request, so it is dropped here with one startup warning.
        let prompt_cache = match (provider.prompt_cache, spec) {
            (Some(retention), Some(spec))
                if !spec.caching.retention.is_empty()
                    && !spec.caching.retention.contains(&retention.into()) =>
            {
                tracing::warn!(
                    "model '{}' does not honour prompt_cache {:?}; its Turns send no cache option",
                    catalog_key,
                    retention
                );
                None
            }
            (prompt_cache, _) => prompt_cache,
        };
        Ok(ModelProfile::builder()
            .provider_id(provider.id.as_str().to_owned())
            .model_id(model.id.clone())
            .display_name(
                model
                    .display_name
                    .clone()
                    .or_else(|| spec.map(|spec| spec.display_name.clone()))
                    .unwrap_or_else(|| model.id.clone()),
            )
            .context_tokens(context_tokens)
            .max_output_tokens(max_output_tokens)
            .request_max_tokens(request_max_tokens)
            .input(input)
            .prompt_cache(prompt_cache)
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

    /// Resolves the provider's shared authenticator through the base transport.
    ///
    /// OAuth traffic never passes request middleware; the authenticator keeps the
    /// live credential, refresh lock and token store for every model of the provider.
    async fn auth_context(
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

    /// Builds a fresh upstream model for one request, sending through `http`.
    ///
    /// Credentials are resolved here so callers never hold them; a new model per
    /// request means no history-adaptation state outlives the request.
    pub async fn connect(
        &self,
        http: DynHttpClient,
    ) -> Result<DynModel<Completion>, ProviderConnectError> {
        let auth = self.auth_context().await?;
        let id = &self.upstream_id;
        let idle = self.idle_timeout;
        Ok(match self.config.clone() {
            ProviderConfig::OpenAi(mut config) => {
                if let Some(auth) = auth {
                    config.api_key = auth.api_key;
                    config.account_id = auth.account_id;
                    if let Some(base) = auth.api_base {
                        config.base_url = base;
                    }
                }
                config
                    .connect(http)
                    .completion(id)
                    .with_idle_timeout(idle)
                    .erase()
            }
            ProviderConfig::Anthropic(config) => config
                .connect(http)
                .completion(id)
                .with_idle_timeout(idle)
                .erase(),
            ProviderConfig::Gemini(config) => config
                .connect(http)
                .completion(id)
                .with_idle_timeout(idle)
                .erase(),
        })
    }

    /// Preserves nested project defaults; upstream additional_params merges only top-level keys.
    pub fn prepare(&self, mut request: CompletionRequest) -> CompletionRequest {
        // Turns send the configured budget; callers with their own cap keep it.
        request
            .max_tokens
            .get_or_insert(self.profile.request_max_tokens);
        if let Some(Reasoning::Effort(requested)) = request.options.reasoning {
            request.options.reasoning =
                Some(Reasoning::Effort(self.supported_effort(requested)));
        }
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

    /// Snaps `requested` to the highest listed tier at or below it, else the lowest
    /// listed tier, so a stronger level never sends less reasoning than a weaker one.
    /// Models without listed tiers keep the request for rig to map.
    fn supported_effort(&self, requested: Effort) -> Effort {
        let rank = |effort: &Effort| {
            EFFORT_ORDER
                .iter()
                .position(|candidate| candidate == effort)
        };
        let Some(limit) = rank(&requested) else {
            return requested;
        };
        let ranked = self.efforts.iter().filter_map(|effort| {
            rank(effort).map(|position| (position, *effort))
        });
        ranked
            .clone()
            .filter(|(position, _)| *position <= limit)
            .max_by_key(|(position, _)| *position)
            .or_else(|| ranked.min_by_key(|(position, _)| *position))
            .map_or(requested, |(_, effort)| effort)
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
            // Clones share the refresh lock and remembered credential across this provider's models.
            let auth =
                provider.auth.as_ref().map(ConfiguredModel::authenticator);
            for model in &provider.models {
                let key = format!("{}/{}", provider.id.as_str(), model.id);
                if models.contains_key(&key) || failures.contains_key(&key) {
                    continue;
                }
                match ConfiguredModel::new(
                    provider,
                    model,
                    &config,
                    http.clone(),
                    auth.clone(),
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
