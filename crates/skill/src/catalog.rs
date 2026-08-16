use std::collections::BTreeMap;
use std::fs;

use prompt::MarkdownDocument;
use protocol::{
    SkillDiagnostic, SkillDiagnosticCode, SkillDiagnosticSeverity, SkillInfo,
};

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

/// Typed result of attempting one pi-compatible Skill command expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillCommandExpansion {
    /// Input is not a command for a known Skill and must continue unchanged.
    NotSkillCommand,
    /// A known Skill was read and expanded successfully.
    Expanded(String),
    /// A known Skill could not be read and the original input must be preserved.
    Failed {
        /// Original user input that continues through Template expansion.
        original: String,
        /// Structured failure suitable for runtime events and clients.
        diagnostic: Box<SkillDiagnostic>,
    },
}

/// Deterministic effective Skill catalog for one Session.
#[derive(Clone)]
pub struct SkillCatalog {
    pub(crate) skills: BTreeMap<String, SkillInfo>,
    pub(crate) diagnostics: Vec<SkillDiagnostic>,
}

impl SkillCatalog {
    /// Returns discovered metadata for one Skill name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SkillInfo> {
        self.skills.get(name)
    }

    /// Reads and expands one selected Skill using the command invocation format.
    pub fn invoke(&self, name: &str) -> Result<String, SkillError> {
        let descriptor = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?;
        Self::expanded_content(descriptor).map_err(SkillError::Io)
    }

    /// Clones descriptors in deterministic name order for read-only clients.
    #[must_use]
    pub fn descriptors(&self) -> Vec<SkillInfo> {
        self.skills.values().cloned().collect()
    }

    /// Returns skipped-rule diagnostics produced while constructing the catalog.
    #[must_use]
    pub fn diagnostics(&self) -> &[SkillDiagnostic] {
        &self.diagnostics
    }

    /// Expands one known Skill command into its body, location, and arguments.
    pub fn expand_command(&self, input: &str) -> SkillCommandExpansion {
        let Ok(invocation) = SkillInvocation::try_from(input) else {
            return SkillCommandExpansion::NotSkillCommand;
        };
        let Some(skill) = self.skills.get(invocation.name) else {
            return SkillCommandExpansion::NotSkillCommand;
        };
        let mut expanded = match Self::expanded_content(skill) {
            Ok(expanded) => expanded,
            Err(error) => {
                return SkillCommandExpansion::Failed {
                    original: input.to_string(),
                    diagnostic: Box::new(
                        SkillDiagnostic::builder()
                            .severity(SkillDiagnosticSeverity::Warning)
                            .code(SkillDiagnosticCode::FileReadFailed)
                            .message(format!(
                                "failed to read Skill file '{}': {error}",
                                skill.path.display()
                            ))
                            .path(Some(skill.path.clone()))
                            .source(Some(skill.source.clone()))
                            .build(),
                    ),
                };
            }
        };
        if !invocation.arguments.is_empty() {
            expanded.push_str("\n\n");
            expanded.push_str(invocation.arguments);
        }

        SkillCommandExpansion::Expanded(expanded)
    }

    /// Reads one Skill and renders its body with stable location context.
    fn expanded_content(skill: &SkillInfo) -> Result<String, std::io::Error> {
        let content = fs::read_to_string(&skill.path)?;
        let document = MarkdownDocument::parse(&content);
        Ok(format!(
            "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
            Self::escape_xml(&skill.name),
            Self::escape_xml(&skill.path.to_string_lossy()),
            Self::escape_xml(&skill.reference_dir.to_string_lossy()),
            document.body().trim()
        ))
    }

    /// Escapes Skill metadata embedded in XML attributes and text.
    fn escape_xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}
