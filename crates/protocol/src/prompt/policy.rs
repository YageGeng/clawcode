use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configured source for one complete or appended System Prompt body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PromptContentSource {
    /// Reads Prompt content from the configured filesystem path.
    Path(PathBuf),
    /// Uses the configured text without filesystem resolution.
    Text(String),
}

/// Immutable policy controlling Prompt resource discovery for new sessions.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(default, rename_all = "snake_case")]
pub struct PromptPolicy {
    /// Whether project and ancestor instruction files are discovered.
    #[builder(default = true)]
    pub load_project_instructions: bool,
    /// Whether global and project Prompt Template directories are discovered.
    #[builder(default = true)]
    pub load_templates: bool,
    /// Optional replacement for the default System Prompt body.
    #[builder(default)]
    pub system_prompt: Option<PromptContentSource>,
    /// Ordered sources appended after the selected System Prompt body.
    #[builder(default)]
    pub append_system_prompts: Vec<PromptContentSource>,
    /// Explicit Prompt Template files or directories.
    #[builder(default)]
    pub template_paths: Vec<PathBuf>,
}

impl Default for PromptPolicy {
    /// Enables pi-compatible automatic instruction and template discovery.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Session-specific inputs used to create one immutable Prompt snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PromptResourceRequest {
    /// Working directory whose project resources may be discovered.
    pub cwd: PathBuf,
    /// Whether project-owned resources passed the trust decision.
    pub project_resources_allowed: bool,
    /// Extension-contributed Prompt Template paths in hook order.
    pub extension_prompt_paths: Vec<PathBuf>,
}

/// Session-specific inputs used to create one immutable Skill snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillResourceRequest {
    /// Working directory whose project Skills may be discovered.
    pub cwd: PathBuf,
    /// Whether project-owned resources passed the trust decision.
    pub project_resources_allowed: bool,
    /// Extension-contributed Skill paths in hook order.
    pub extension_skill_paths: Vec<PathBuf>,
}
