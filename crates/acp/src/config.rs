use std::fmt;

use agent_client_protocol::schema::v2 as wire;
use protocol::{ModelProfile, SessionSettings, ThinkingLevel};

/// Config id of the model select.
const MODEL: &str = "model";
/// Config id of the thinking-level select.
const THINKING_LEVEL: &str = "thinking_level";

/// Borrows one model profile for its `"{provider_id}/{model_id}"` option value.
struct ModelValue<'a>(&'a ModelProfile);

impl fmt::Display for ModelValue<'_> {
    /// Joins provider and model; compared whole, so a `/` in the model id stays unambiguous.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.0.provider_id, self.0.model_id)
    }
}

/// Borrows one thinking level for its wire value and display name.
struct ThinkingValue(ThinkingLevel);

impl ThinkingValue {
    /// Returns the serde name clients send back, such as `x_high`.
    fn id(&self) -> String {
        serde_json::to_value(self.0)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Returns the human-readable option name.
    fn name(&self) -> &'static str {
        match self.0 {
            ThinkingLevel::Off => "Off",
            ThinkingLevel::Minimal => "Minimal",
            ThinkingLevel::Low => "Low",
            ThinkingLevel::Medium => "Medium",
            ThinkingLevel::High => "High",
            ThinkingLevel::XHigh => "Extra high",
        }
    }
}

/// Borrows Session settings for their ACP config options.
pub(crate) struct ConfigOptionsOf<'a>(pub(crate) &'a SessionSettings);

impl From<ConfigOptionsOf<'_>> for Vec<wire::SessionConfigOption> {
    /// Lists the model select first, then thinking level.
    fn from(settings: ConfigOptionsOf<'_>) -> Self {
        let settings = settings.0;
        let models = settings
            .models
            .iter()
            .map(|profile| {
                wire::SessionConfigSelectOption::new(
                    ModelValue(profile).to_string(),
                    profile.display_name.clone(),
                )
                .description(format!(
                    "{} · {} tokens context",
                    profile.provider_id, profile.context_tokens
                ))
            })
            .collect::<Vec<_>>();
        // shortcut: ModelProfile has no reasoning capability, so every model offers
        // every level; filter per model once providers report it.
        let levels = ThinkingLevel::ALL
            .into_iter()
            .map(|level| {
                let value = ThinkingValue(level);
                wire::SessionConfigSelectOption::new(value.id(), value.name())
            })
            .collect::<Vec<_>>();
        vec![
            wire::SessionConfigOption::select(
                MODEL,
                "Model",
                ModelValue(&settings.model).to_string(),
                models,
            )
            .category(wire::SessionConfigOptionCategory::Model),
            wire::SessionConfigOption::select(
                THINKING_LEVEL,
                "Thinking",
                ThinkingValue(settings.thinking_level).id(),
                levels,
            )
            .category(wire::SessionConfigOptionCategory::ThoughtLevel),
        ]
    }
}

/// One validated change a client asked for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConfigChange {
    Model { provider: String, model_id: String },
    ThinkingLevel(ThinkingLevel),
}

impl TryFrom<(&SessionSettings, &wire::SetSessionConfigOptionRequest)>
    for ConfigChange
{
    type Error = agent_client_protocol::Error;

    /// Accepts only ids and values the Session's options offer; anything else is -32602.
    fn try_from(
        (settings, request): (
            &SessionSettings,
            &wire::SetSessionConfigOptionRequest,
        ),
    ) -> Result<Self, Self::Error> {
        let invalid = |message: String| {
            tracing::warn!("rejected session/set_config_option: {}", message);
            agent_client_protocol::Error::invalid_params().data(message)
        };
        let config_id = request.config_id.to_string();
        let wire::SessionConfigOptionValue::Id { value } = &request.value
        else {
            return Err(invalid(format!("{config_id} takes a select value")));
        };
        let value = value.to_string();
        match config_id.as_str() {
            MODEL => settings
                .models
                .iter()
                .find(|profile| ModelValue(profile).to_string() == value)
                .map(|profile| Self::Model {
                    provider: profile.provider_id.clone(),
                    model_id: profile.model_id.clone(),
                })
                .ok_or_else(|| {
                    invalid(format!("model is not offered: {value}"))
                }),
            THINKING_LEVEL => ThinkingLevel::ALL
                .into_iter()
                .find(|level| ThinkingValue(*level).id() == value)
                .map(Self::ThinkingLevel)
                .ok_or_else(|| {
                    invalid(format!("thinking level is not offered: {value}"))
                }),
            other => Err(invalid(format!("unknown config option: {other}"))),
        }
    }
}
