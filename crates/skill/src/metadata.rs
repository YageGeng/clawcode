use std::path::Path;

use prompt::MarkdownDocument;
use protocol::{
    SkillDiagnostic, SkillDiagnosticCode, SkillDiagnosticSeverity, SkillSource,
};
use serde::Deserialize;

const MAX_SKILL_NAME_CHARS: usize = 64;
const MAX_SKILL_DESCRIPTION_CHARS: usize = 1_024;

/// YAML fields accepted from one Skill frontmatter block.
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

/// Validated metadata retained for one loadable Skill.
pub(crate) struct SkillMetadata {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) disable_model_invocation: bool,
}

impl SkillMetadata {
    /// Parses frontmatter and returns load-preserving validation warnings.
    pub(crate) fn parse(
        document: &MarkdownDocument,
        path: &Path,
        source: &SkillSource,
    ) -> Result<(Self, Vec<SkillDiagnostic>), Box<SkillDiagnostic>> {
        if !document.has_frontmatter() {
            return Err(Box::new(
                SkillDiagnostic::builder()
                    .severity(SkillDiagnosticSeverity::Warning)
                    .code(SkillDiagnosticCode::FrontmatterInvalid)
                    .message(format!(
                        "Skill frontmatter is missing or incomplete: {}",
                        path.display()
                    ))
                    .path(Some(path.to_path_buf()))
                    .source(Some(source.clone()))
                    .build(),
            ));
        }
        let frontmatter: SkillFrontmatter =
            document.metadata().map_err(|error| {
                Box::new(
                    SkillDiagnostic::builder()
                        .severity(SkillDiagnosticSeverity::Warning)
                        .code(SkillDiagnosticCode::FrontmatterInvalid)
                        .message(format!(
                            "invalid Skill frontmatter '{}': {error}",
                            path.display()
                        ))
                        .path(Some(path.to_path_buf()))
                        .source(Some(source.clone()))
                        .build(),
                )
            })?;
        let name = frontmatter
            .name
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(|value| value.to_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                Box::new(Self::metadata_error(
                    path,
                    source,
                    "missing Skill name",
                ))
            })?;
        let description = frontmatter
            .description
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                Box::new(Self::metadata_error(
                    path,
                    source,
                    "missing Skill description",
                ))
            })?;

        let mut diagnostics = Vec::new();
        if !Self::valid_name(&name) {
            diagnostics.push(Self::metadata_error(
                path,
                source,
                &format!(
                    "Skill name '{name}' must contain at most {MAX_SKILL_NAME_CHARS} lowercase letters, digits, or non-consecutive interior hyphens"
                ),
            ));
        }
        if description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS {
            diagnostics.push(Self::metadata_error(
                path,
                source,
                &format!(
                    "Skill description exceeds {MAX_SKILL_DESCRIPTION_CHARS} characters"
                ),
            ));
        }

        Ok((
            Self {
                name,
                description,
                disable_model_invocation: frontmatter.disable_model_invocation,
            },
            diagnostics,
        ))
    }

    /// Validates pi's lowercase, numeric, and single-hyphen name grammar.
    fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.chars().count() <= MAX_SKILL_NAME_CHARS
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--")
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
            })
    }

    /// Builds one load-time metadata warning with stable code and provenance.
    fn metadata_error(
        path: &Path,
        source: &SkillSource,
        message: &str,
    ) -> SkillDiagnostic {
        SkillDiagnostic::builder()
            .severity(SkillDiagnosticSeverity::Warning)
            .code(SkillDiagnosticCode::MetadataInvalid)
            .message(message.to_string())
            .path(Some(path.to_path_buf()))
            .source(Some(source.clone()))
            .build()
    }
}
