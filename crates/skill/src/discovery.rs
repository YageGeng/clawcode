use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use prompt::MarkdownDocument;
use protocol::SkillInfo;
use serde::Deserialize;

use crate::SkillError;

/// YAML fields used to derive effective Skill metadata.
#[derive(Debug, Default, Deserialize)]
struct SkillMetadata {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

/// Recursively discovers Skills while stopping below each Skill directory.
#[derive(Default)]
pub(crate) struct SkillDiscovery {
    skills: BTreeMap<String, SkillInfo>,
    diagnostics: Vec<String>,
}

impl SkillDiscovery {
    /// Visits one root or descendant while isolating malformed automatic resources.
    pub(crate) fn visit(&mut self, path: &Path, include_root_files: bool) {
        if path.is_file() {
            let is_skill_file =
                path.file_name().is_some_and(|name| name == "SKILL.md")
                    || (include_root_files
                        && path.extension().is_some_and(|value| value == "md"));
            if is_skill_file && let Err(error) = self.insert(path) {
                self.diagnostics.push(format!(
                    "invalid Skill '{}': {error}",
                    path.display()
                ));
            }
            return;
        }

        let skill_file = path.join("SKILL.md");
        if skill_file.is_file() {
            if let Err(error) = self.insert(&skill_file) {
                self.diagnostics.push(format!(
                    "invalid Skill '{}': {error}",
                    skill_file.display()
                ));
            }
            return;
        }

        let mut builder = WalkBuilder::new(path);
        builder
            .follow_links(true)
            .parents(false)
            .require_git(false)
            .git_global(false)
            .git_exclude(false)
            .add_custom_ignore_filename(".fdignore")
            .sort_by_file_name(|left, right| left.cmp(right))
            .filter_entry(|entry| {
                if entry.depth() == 0 {
                    return true;
                }
                let name = entry.file_name().to_string_lossy();
                if name == "node_modules" || name.starts_with('.') {
                    return false;
                }
                if entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_dir())
                    && entry
                        .path()
                        .parent()
                        .is_some_and(|parent| parent.join("SKILL.md").is_file())
                {
                    return false;
                }
                true
            });
        for entry in builder.build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.diagnostics.push(format!(
                        "failed to discover Skill under '{}': {error}",
                        path.display()
                    ));
                    continue;
                }
            };
            if entry.depth() == 0
                || !entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            let is_skill_file = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name == "SKILL.md");
            let is_root_markdown = include_root_files
                && entry.depth() == 1
                && entry.path().extension().is_some_and(|value| value == "md");
            if (is_skill_file || is_root_markdown)
                && let Err(error) = self.insert(entry.path())
            {
                self.diagnostics.push(format!(
                    "invalid Skill '{}': {error}",
                    entry.path().display()
                ));
            }
        }
    }

    /// Returns discovered metadata and recoverable diagnostics after traversal.
    pub(crate) fn into_parts(
        self,
    ) -> (BTreeMap<String, SkillInfo>, Vec<String>) {
        (self.skills, self.diagnostics)
    }

    /// Parses and inserts one canonical SKILL.md descriptor.
    fn insert(&mut self, path: &Path) -> Result<(), SkillError> {
        let canonical_path = fs::canonicalize(path)?;
        let content = fs::read_to_string(&canonical_path)?;
        let document = MarkdownDocument::parse(&content);
        let metadata: SkillMetadata = document.metadata().map_err(|error| {
            SkillError::InvalidFrontmatter {
                path: canonical_path.clone(),
                reason: error.to_string(),
            }
        })?;
        let name = metadata
            .name
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                canonical_path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|value| value.to_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| SkillError::InvalidFrontmatter {
                path: canonical_path.clone(),
                reason: "missing name".to_string(),
            })?;
        let description = metadata
            .description
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| SkillError::InvalidFrontmatter {
                path: canonical_path.clone(),
                reason: "missing description".to_string(),
            })?;
        if let Some(winner) = self.skills.get(&name) {
            self.diagnostics.push(format!(
                "Skill name '{name}' collision: keeping '{}' and ignoring '{}'",
                winner.path.display(),
                canonical_path.display()
            ));
            return Ok(());
        }
        self.skills.insert(
            name.clone(),
            SkillInfo::builder()
                .name(name)
                .description(description)
                .path(canonical_path)
                .disable_model_invocation(metadata.disable_model_invocation)
                .build(),
        );
        Ok(())
    }
}

/// Resolves a configured Skill path against the Session working directory.
pub(crate) fn resolve_skill_path(path: &Path, cwd: &Path) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    fs::canonicalize(&resolved).unwrap_or(resolved)
}
