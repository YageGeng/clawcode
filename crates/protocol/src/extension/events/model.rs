use serde::{Deserialize, Serialize};

use crate::ModelProfile;

/// Configurable model reasoning level following Pi's ordered levels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// Disable explicit reasoning when supported.
    Off,
    /// Minimal provider reasoning budget.
    Minimal,
    /// Low provider reasoning budget.
    Low,
    /// Medium provider reasoning budget.
    Medium,
    /// High provider reasoning budget.
    High,
    /// Extra-high provider reasoning budget.
    XHigh,
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
