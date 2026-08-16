use std::path::Path;

use protocol::{ProjectInstruction, PromptDiagnostic, PromptTemplateInfo};

use crate::PromptTemplateCatalog;

/// Immutable Prompt resource snapshot owned by one Kernel Session.
#[derive(Debug, typed_builder::TypedBuilder)]
pub struct PromptSession {
    cwd: std::path::PathBuf,
    #[builder(default)]
    custom_prompt: Option<String>,
    #[builder(default)]
    append_system_prompt: Option<String>,
    #[builder(default)]
    instructions: Vec<ProjectInstruction>,
    templates: PromptTemplateCatalog,
    #[builder(default)]
    diagnostics: Vec<PromptDiagnostic>,
}

impl PromptSession {
    /// Returns the normalized Session working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Returns the selected custom System Prompt body when one exists.
    #[must_use]
    pub fn custom_prompt(&self) -> Option<&str> {
        self.custom_prompt.as_deref()
    }

    /// Returns the selected or combined Append System Prompt content.
    #[must_use]
    pub fn append_system_prompt(&self) -> Option<&str> {
        self.append_system_prompt.as_deref()
    }

    /// Returns ordered global and project instruction resources.
    #[must_use]
    pub fn instructions(&self) -> &[ProjectInstruction] {
        &self.instructions
    }

    /// Returns effective Prompt Template metadata in source order.
    #[must_use]
    pub fn templates(&self) -> Vec<PromptTemplateInfo> {
        self.templates.templates()
    }

    /// Returns all source warnings and collisions encountered at Session startup.
    #[must_use]
    pub fn diagnostics(&self) -> &[PromptDiagnostic] {
        &self.diagnostics
    }

    /// Expands one matching Prompt Template slash command.
    #[must_use]
    pub fn expand_template(&self, input: &str) -> String {
        self.templates.expand(input)
    }
}
