use serde::{Deserialize, Serialize};

use crate::PromptSourceInfo;

/// Public metadata for one effective Prompt Template command.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "snake_case")]
pub struct PromptTemplateInfo {
    /// Slash-command name derived from the Markdown filename.
    pub name: String,
    /// Human-readable command purpose.
    pub description: String,
    /// Optional argument shape displayed by protocol clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub argument_hint: Option<String>,
    /// Winning resource source after collision resolution.
    pub source: PromptSourceInfo,
}
