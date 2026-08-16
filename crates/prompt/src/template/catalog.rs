use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use protocol::{
    PromptCollision, PromptDiagnostic, PromptDiagnosticSeverity,
    PromptSourceInfo, PromptSourceKind, PromptTemplateInfo,
};
use serde::Deserialize;

use crate::PromptError;

use super::document::MarkdownDocument;
use super::expansion::TemplateInvocation;
use super::{PromptTemplateRoot, PromptTemplateRootRequirement};

/// Frontmatter fields recognized for Prompt Template command metadata.
#[derive(Debug, Default, Deserialize)]
struct PromptTemplateMetadata {
    #[serde(default)]
    description: String,
    #[serde(default, rename = "argument-hint")]
    argument_hint: Option<String>,
}

/// One effective Prompt Template and its expansion body.
#[derive(Debug, Clone)]
struct PromptTemplate {
    info: PromptTemplateInfo,
    content: String,
}

/// Immutable, insertion-ordered Prompt Template catalog for one session.
#[derive(Debug, Default)]
pub struct PromptTemplateCatalog {
    templates: Vec<PromptTemplate>,
    by_name: BTreeMap<String, usize>,
    diagnostics: Vec<PromptDiagnostic>,
}

impl PromptTemplateCatalog {
    /// Discovers ordered Prompt Template roots and keeps the first duplicate name.
    pub fn discover(roots: &[PromptTemplateRoot]) -> Result<Self, PromptError> {
        let mut catalog = Self::default();

        for root in roots {
            catalog.load_root(root)?;
        }

        Ok(catalog)
    }

    /// Returns effective Prompt Template metadata in source insertion order.
    #[must_use]
    pub fn templates(&self) -> Vec<PromptTemplateInfo> {
        self.templates
            .iter()
            .map(|template| template.info.clone())
            .collect()
    }

    /// Returns discovery warnings and collisions in encounter order.
    #[must_use]
    pub fn diagnostics(&self) -> &[PromptDiagnostic] {
        &self.diagnostics
    }

    /// Expands a matching slash command or preserves unrecognized input.
    #[must_use]
    pub fn expand(&self, input: &str) -> String {
        let Ok(invocation) = TemplateInvocation::try_from(input) else {
            return input.to_string();
        };
        let Some(index) = self.by_name.get(invocation.name) else {
            return input.to_string();
        };
        let Some(template) = self.templates.get(*index) else {
            return input.to_string();
        };

        invocation.arguments.expand(&template.content)
    }

    /// Loads one required or automatically discovered root according to policy.
    fn load_root(
        &mut self,
        root: &PromptTemplateRoot,
    ) -> Result<(), PromptError> {
        let metadata = match fs::metadata(&root.path) {
            Ok(metadata) => metadata,
            // Default Prompt directories are optional and their absence is not diagnostic.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && root.requirement
                        == PromptTemplateRootRequirement::Discovered =>
            {
                return Ok(());
            }
            Err(error) => {
                return self.handle_root_error(root, error.to_string());
            }
        };

        if metadata.is_file() {
            if root.path.extension().is_some_and(|value| value == "md") {
                self.load_file(&root.path, root);
            }
            return Ok(());
        }
        if !metadata.is_dir() {
            return self.handle_root_error(
                root,
                "path is neither a regular file nor a directory".to_string(),
            );
        }

        let entries = match fs::read_dir(&root.path) {
            Ok(entries) => entries,
            Err(error) => {
                return self.handle_root_error(root, error.to_string());
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(error) => self.push_warning(
                    &root.path,
                    root,
                    format!("failed to read Prompt Template directory entry: {error}"),
                ),
            }
        }
        paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

        for path in paths {
            let symlink_metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    self.push_warning(
                        &path,
                        root,
                        format!("failed to inspect Prompt Template: {error}"),
                    );
                    continue;
                }
            };
            let is_file = if symlink_metadata.file_type().is_symlink() {
                match fs::metadata(&path) {
                    Ok(metadata) => metadata.is_file(),
                    Err(error) => {
                        self.push_warning(
                            &path,
                            root,
                            format!("failed to resolve Prompt Template symlink: {error}"),
                        );
                        false
                    }
                }
            } else {
                symlink_metadata.is_file()
            };
            if is_file && path.extension().is_some_and(|value| value == "md") {
                self.load_file(&path, root);
            }
        }

        Ok(())
    }

    /// Applies the root requirement policy to an unavailable path.
    fn handle_root_error(
        &mut self,
        root: &PromptTemplateRoot,
        reason: String,
    ) -> Result<(), PromptError> {
        match root.requirement {
            PromptTemplateRootRequirement::Required => {
                Err(PromptError::ConfiguredResource {
                    path: root.path.clone(),
                    reason,
                })
            }
            PromptTemplateRootRequirement::Discovered => {
                self.push_warning(
                    &root.path,
                    root,
                    format!(
                        "failed to discover Prompt Template root: {reason}"
                    ),
                );
                Ok(())
            }
        }
    }

    /// Parses and inserts one readable Markdown Prompt Template.
    fn load_file(&mut self, path: &Path, root: &PromptTemplateRoot) {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) => {
                self.push_warning(
                    path,
                    root,
                    format!("failed to read Prompt Template: {error}"),
                );
                return;
            }
        };
        let document = MarkdownDocument::parse(&content);
        let metadata = match document.metadata::<PromptTemplateMetadata>() {
            Ok(metadata) => metadata,
            Err(error) => {
                self.push_warning(path, root, error.to_string());
                return;
            }
        };
        let Some(name) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_string)
        else {
            self.push_warning(
                path,
                root,
                "Prompt Template filename is not valid UTF-8".to_string(),
            );
            return;
        };
        let source = PromptSourceInfo {
            kind: PromptSourceKind::Template,
            scope: root.scope,
            path: Some(path.to_path_buf()),
        };
        if let Some(index) = self.by_name.get(&name)
            && let Some(winner) = self.templates.get(*index)
        {
            let collision = PromptCollision {
                name: name.clone(),
                winner: winner.info.source.clone(),
                loser: source.clone(),
            };
            self.diagnostics.push(
                PromptDiagnostic::builder()
                    .severity(PromptDiagnosticSeverity::Collision)
                    .message(format!(
                        "Prompt Template '{name}' is shadowed by an earlier source"
                    ))
                    .source(source)
                    .collision(collision)
                    .build(),
            );
            return;
        }

        let description = if metadata.description.is_empty() {
            document
                .body()
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    let mut value = line.chars().take(60).collect::<String>();
                    if line.chars().count() > 60 {
                        value.push_str("...");
                    }
                    value
                })
                .unwrap_or_default()
        } else {
            metadata.description
        };
        let argument_hint =
            metadata.argument_hint.filter(|value| !value.is_empty());
        let info = PromptTemplateInfo::builder()
            .name(name.clone())
            .description(description)
            .argument_hint(argument_hint)
            .source(source)
            .build();
        self.by_name.insert(name, self.templates.len());
        self.templates.push(PromptTemplate {
            info,
            content: document.body().to_string(),
        });
    }

    /// Records one source-scoped warning without Prompt body content.
    fn push_warning(
        &mut self,
        path: &Path,
        root: &PromptTemplateRoot,
        message: String,
    ) {
        self.diagnostics.push(
            PromptDiagnostic::builder()
                .severity(PromptDiagnosticSeverity::Warning)
                .message(message)
                .source(PromptSourceInfo {
                    kind: PromptSourceKind::Template,
                    scope: root.scope,
                    path: Some(PathBuf::from(path)),
                })
                .build(),
        );
    }
}
