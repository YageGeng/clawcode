use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ProjectInstruction, SkillInfo};

/// Optional System Prompt text contributed by one registered tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPromptContribution {
    /// Concise tool capability shown in the available-tools section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Tool-specific operating guidelines appended to the base guidance.
    pub guidelines: Vec<String>,
}

/// One selected tool and its explicit System Prompt contribution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SystemPromptTool {
    /// Registered tool name used by the current Turn.
    pub name: String,
    /// Optional snippet and guidelines supplied by the tool implementation.
    pub contribution: ToolPromptContribution,
}

/// Complete structured inputs used to build one Turn's System Prompt.
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
pub struct SystemPromptBuildOptions {
    /// Session working directory rendered into the final Prompt.
    pub cwd: PathBuf,
    /// Optional custom body replacing the built-in identity text.
    #[builder(default)]
    pub custom_prompt: Option<String>,
    /// Optional combined content appended to the selected body.
    #[builder(default)]
    pub append_system_prompt: Option<String>,
    /// Tool names selected for the current Turn.
    #[builder(default)]
    pub selected_tools: Vec<String>,
    /// Prompt contributions indexed by selected tool name.
    #[builder(default)]
    pub tool_contributions: BTreeMap<String, ToolPromptContribution>,
    /// Ordered global and project instructions.
    #[builder(default)]
    pub project_instructions: Vec<ProjectInstruction>,
    /// Effective session Skill metadata.
    #[builder(default)]
    pub skills: Vec<SkillInfo>,
    /// Whether Skill usage instructions are included when supported by tools.
    pub include_skill_instructions: bool,
}
