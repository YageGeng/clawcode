use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use prompt::MarkdownDocument;
use protocol::{
    SkillCollision, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiagnosticSeverity, SkillDiscoveryMode, SkillInfo,
};

use crate::metadata::SkillMetadata;
use crate::source::SkillRoot;

/// Discovers Skills in source order while retaining recoverable diagnostics.
#[derive(Default)]
pub(crate) struct SkillDiscovery {
    skills: BTreeMap<String, SkillInfo>,
    real_paths: HashSet<std::path::PathBuf>,
    diagnostics: Vec<SkillDiagnostic>,
}

impl SkillDiscovery {
    /// Visits one typed source using its required-path and traversal semantics.
    pub(crate) fn visit(&mut self, source: &SkillRoot) {
        let metadata = match fs::metadata(&source.info.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if source.required {
                    self.diagnostics.push(
                        SkillDiagnostic::builder()
                            .severity(SkillDiagnosticSeverity::Error)
                            .code(SkillDiagnosticCode::PathNotFound)
                            .message(format!(
                                "Skill path does not exist: {}",
                                source.info.root.display()
                            ))
                            .path(Some(source.info.root.clone()))
                            .source(Some(source.info.clone()))
                            .build(),
                    );
                }
                return;
            }
            Err(error) => {
                self.diagnostics.push(
                    SkillDiagnostic::builder()
                        .severity(SkillDiagnosticSeverity::Warning)
                        .code(SkillDiagnosticCode::FileInfoFailed)
                        .message(format!(
                            "failed to inspect Skill path '{}': {error}",
                            source.info.root.display()
                        ))
                        .path(Some(source.info.root.clone()))
                        .source(Some(source.info.clone()))
                        .build(),
                );
                return;
            }
        };

        if metadata.is_file() {
            if source
                .info
                .root
                .extension()
                .is_some_and(|value| value == "md")
            {
                self.insert(&source.info.root, source);
            } else {
                self.unsupported_path(source);
            }
            return;
        }
        if !metadata.is_dir() {
            self.unsupported_path(source);
            return;
        }

        self.visit_directory(source);
    }

    /// Returns conflict-resolved metadata and diagnostics after all sources finish.
    pub(crate) fn into_parts(
        self,
    ) -> (BTreeMap<String, SkillInfo>, Vec<SkillDiagnostic>) {
        (self.skills, self.diagnostics)
    }

    /// Traverses one source directory without descending below a Skill root.
    fn visit_directory(&mut self, source: &SkillRoot) {
        // Pi treats a visible root SKILL.md as the complete directory source.
        let mut root_builder = Self::walk_builder(source);
        root_builder.max_depth(Some(1));
        let root_skill =
            root_builder.build().filter_map(Result::ok).find(|entry| {
                entry.depth() == 1
                    && entry
                        .file_type()
                        .is_some_and(|file_type| file_type.is_file())
                    && entry.file_name() == "SKILL.md"
            });
        if let Some(root_skill) = root_skill {
            self.insert(root_skill.path(), source);
            return;
        }

        for entry in Self::walk_builder(source).build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.diagnostics.push(
                        SkillDiagnostic::builder()
                            .severity(SkillDiagnosticSeverity::Warning)
                            .code(SkillDiagnosticCode::DirectoryReadFailed)
                            .message(format!(
                                "failed to discover Skill under '{}': {error}",
                                source.info.root.display()
                            ))
                            .path(Some(source.info.root.clone()))
                            .source(Some(source.info.clone()))
                            .build(),
                    );
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
            let nested_skill = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name == "SKILL.md");
            let root_markdown = source.info.discovery_mode
                == SkillDiscoveryMode::Pi
                && entry.depth() == 1
                && entry.path().extension().is_some_and(|value| value == "md");
            if nested_skill || root_markdown {
                self.insert(entry.path(), source);
            }
        }
    }

    /// Builds the ignore-aware walker shared by root detection and traversal.
    fn walk_builder(source: &SkillRoot) -> WalkBuilder {
        let mut builder = WalkBuilder::new(&source.info.root);
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
                !(entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_dir())
                    && entry.path().parent().is_some_and(|parent| {
                        parent.join("SKILL.md").is_file()
                    }))
            });
        builder
    }

    /// Parses, validates, de-duplicates, and inserts one Skill candidate.
    fn insert(&mut self, path: &Path, source: &SkillRoot) {
        let canonical_path = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) => {
                self.diagnostics.push(
                    SkillDiagnostic::builder()
                        .severity(SkillDiagnosticSeverity::Warning)
                        .code(SkillDiagnosticCode::FileInfoFailed)
                        .message(format!(
                            "failed to resolve Skill file '{}': {error}",
                            path.display()
                        ))
                        .path(Some(path.to_path_buf()))
                        .source(Some(source.info.clone()))
                        .build(),
                );
                return;
            }
        };
        if self.real_paths.contains(&canonical_path) {
            return;
        }
        let content = match fs::read_to_string(&canonical_path) {
            Ok(content) => content,
            Err(error) => {
                self.diagnostics.push(
                    SkillDiagnostic::builder()
                        .severity(SkillDiagnosticSeverity::Warning)
                        .code(SkillDiagnosticCode::FileReadFailed)
                        .message(format!(
                            "failed to read Skill file '{}': {error}",
                            canonical_path.display()
                        ))
                        .path(Some(canonical_path))
                        .source(Some(source.info.clone()))
                        .build(),
                );
                return;
            }
        };
        let document = MarkdownDocument::parse(&content);
        let (metadata, mut diagnostics) = match SkillMetadata::parse(
            &document,
            &canonical_path,
            &source.info,
        ) {
            Ok(result) => result,
            Err(diagnostic) => {
                self.diagnostics.push(*diagnostic);
                return;
            }
        };
        self.diagnostics.append(&mut diagnostics);

        if let Some(winner) = self.skills.get(&metadata.name) {
            self.diagnostics.push(
                SkillDiagnostic::builder()
                    .severity(SkillDiagnosticSeverity::Warning)
                    .code(SkillDiagnosticCode::NameCollision)
                    .message(format!(
                        "Skill name '{}' collision: keeping '{}' and ignoring '{}'",
                        metadata.name,
                        winner.path.display(),
                        canonical_path.display()
                    ))
                    .path(Some(canonical_path.clone()))
                    .source(Some(source.info.clone()))
                    .collision(Some(SkillCollision {
                        name: metadata.name,
                        winner_path: winner.path.clone(),
                        loser_path: canonical_path,
                    }))
                    .build(),
            );
            return;
        }

        let reference_dir = canonical_path
            .parent()
            .unwrap_or(canonical_path.as_path())
            .to_path_buf();
        self.real_paths.insert(canonical_path.clone());
        self.skills.insert(
            metadata.name.clone(),
            SkillInfo::builder()
                .name(metadata.name)
                .description(metadata.description)
                .path(canonical_path)
                .reference_dir(reference_dir)
                .source(source.info.clone())
                .disable_model_invocation(metadata.disable_model_invocation)
                .build(),
        );
    }

    /// Records a required or discovered path whose filesystem type is unsupported.
    fn unsupported_path(&mut self, source: &SkillRoot) {
        self.diagnostics.push(
            SkillDiagnostic::builder()
                .severity(if source.required {
                    SkillDiagnosticSeverity::Error
                } else {
                    SkillDiagnosticSeverity::Warning
                })
                .code(SkillDiagnosticCode::UnsupportedPath)
                .message(format!(
                    "Skill path is not a Markdown file or directory: {}",
                    source.info.root.display()
                ))
                .path(Some(source.info.root.clone()))
                .source(Some(source.info.clone()))
                .build(),
        );
    }
}
