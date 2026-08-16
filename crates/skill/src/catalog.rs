use std::collections::BTreeMap;
use std::fs;

use prompt::MarkdownDocument;
use protocol::SkillInfo;

use crate::SkillError;

/// Parsed `/skill:name args` invocation using pi's literal-space delimiter.
struct SkillInvocation<'a> {
    name: &'a str,
    arguments: &'a str,
}

impl<'a> TryFrom<&'a str> for SkillInvocation<'a> {
    type Error = ();

    /// Parses one Skill command without treating tabs or newlines as delimiters.
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        let command = value.strip_prefix("/skill:").ok_or(())?;
        let (name, arguments) = match command.find(' ') {
            Some(index) => (
                command.get(..index).ok_or(())?,
                command
                    .get(index.saturating_add(1)..)
                    .unwrap_or_default()
                    .trim(),
            ),
            None => (command, ""),
        };
        if name.is_empty() {
            return Err(());
        }

        Ok(Self { name, arguments })
    }
}

/// Deterministic effective Skill catalog for one Session.
#[derive(Clone)]
pub struct SkillCatalog {
    pub(crate) skills: BTreeMap<String, SkillInfo>,
    pub(crate) diagnostics: Vec<String>,
}

impl SkillCatalog {
    /// Returns discovered metadata for one Skill name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SkillInfo> {
        self.skills.get(name)
    }

    /// Reads and returns the complete selected SKILL.md file.
    pub fn invoke(&self, name: &str) -> Result<String, SkillError> {
        let descriptor = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?;
        fs::read_to_string(&descriptor.path).map_err(SkillError::Io)
    }

    /// Clones descriptors in deterministic name order for read-only clients.
    #[must_use]
    pub fn descriptors(&self) -> Vec<SkillInfo> {
        self.skills.values().cloned().collect()
    }

    /// Returns skipped-rule diagnostics produced while constructing the catalog.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Expands one known Skill command into its body, location, and arguments.
    pub fn expand_command(
        &self,
        input: &str,
    ) -> Result<Option<String>, SkillError> {
        let Ok(invocation) = SkillInvocation::try_from(input) else {
            return Ok(None);
        };
        let Some(skill) = self.skills.get(invocation.name) else {
            return Ok(None);
        };
        let content = fs::read_to_string(&skill.path)?;
        let document = MarkdownDocument::parse(&content);
        let base_directory =
            skill.path.parent().unwrap_or(skill.path.as_path());
        let mut expanded = format!(
            "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
            skill.name,
            skill.path.display(),
            base_directory.display(),
            document.body().trim()
        );
        if !invocation.arguments.is_empty() {
            expanded.push_str("\n\n");
            expanded.push_str(invocation.arguments);
        }

        Ok(Some(expanded))
    }
}
