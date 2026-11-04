use serde::{Deserialize, Serialize};

use crate::ModelProfile;

/// Configurable model reasoning level following Pi's ordered levels.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// Send no reasoning option; the model default and configured parameters decide.
    Off,
    /// Minimal provider reasoning budget.
    Minimal,
    /// Low provider reasoning budget.
    Low,
    /// Medium provider reasoning budget; the level a session starts at.
    #[default]
    Medium,
    /// High provider reasoning budget.
    High,
    /// Extra-high provider reasoning budget.
    XHigh,
}

impl ThinkingLevel {
    /// Every level from least to most reasoning.
    pub const ALL: [Self; 6] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
    ];

    /// Returns the portable reasoning effort for this level, or `None` for Off.
    ///
    /// Callers skip unsupported efforts instead of failing the request, because
    /// providers accept different effort sets (DeepSeek takes only low, high and max).
    pub fn effort(self) -> Option<rig_core::completion::options::Effort> {
        use rig_core::completion::options::Effort;
        match self {
            Self::Off => None,
            Self::Minimal => Some(Effort::Minimal),
            Self::Low => Some(Effort::Low),
            Self::Medium => Some(Effort::Medium),
            Self::High => Some(Effort::High),
            Self::XHigh => Some(Effort::XHigh),
        }
    }
}

/// Source of an active-model change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelectSource {
    /// Direct host or extension selection.
    Set,
    /// User cycled through available models.
    Cycle,
    /// Persisted session selection was restored.
    Restore,
}

/// Active model selection changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSelectEvent {
    /// Newly active model.
    pub model: ModelProfile,
    /// Previously active model, when one existed.
    pub previous_model: Option<ModelProfile>,
    /// Operation that selected the new model.
    pub source: ModelSelectSource,
}

/// Active reasoning level changed or was clamped for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingLevelSelectEvent {
    /// Newly active level.
    pub level: ThinkingLevel,
    /// Previously active level.
    pub previous_level: ThinkingLevel,
}
