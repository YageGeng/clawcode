use std::fs;
use std::path::{Path, PathBuf};

use protocol::{
    PromptContentSource, PromptDiagnostic, PromptDiagnosticSeverity,
    PromptPolicy, PromptSourceInfo, PromptSourceKind, PromptSourceScope,
};

use crate::PromptError;

/// Resolved System Prompt and append content plus non-fatal diagnostics.
pub(crate) struct DiscoveredPromptSources {
    pub(crate) custom_prompt: Option<String>,
    pub(crate) append_system_prompt: Option<String>,
    pub(crate) diagnostics: Vec<PromptDiagnostic>,
}

/// Reads an explicit content source relative to one Session working directory.
trait PromptContentSourceReader {
    fn read_from(&self, cwd: &Path) -> Result<String, PromptError>;
}

impl PromptContentSourceReader for PromptContentSource {
    /// Resolves text directly and validates file-backed content as a regular file.
    fn read_from(&self, cwd: &Path) -> Result<String, PromptError> {
        match self {
            Self::Text(content) => Ok(content.clone()),
            Self::Path(configured_path) => {
                let path = if configured_path.is_absolute() {
                    configured_path.clone()
                } else {
                    cwd.join(configured_path)
                };
                let metadata = fs::metadata(&path).map_err(|error| {
                    PromptError::ConfiguredResource {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?;
                if !metadata.is_file() {
                    return Err(PromptError::ConfiguredResource {
                        path,
                        reason: "path is not a regular file".to_string(),
                    });
                }
                fs::read_to_string(&path).map_err(|error| {
                    PromptError::ConfiguredResource {
                        path,
                        reason: error.to_string(),
                    }
                })
            }
        }
    }
}

/// Session-scoped System and Append System source selector.
pub(crate) struct PromptSourceDiscovery {
    global_root: PathBuf,
    cwd: PathBuf,
    project_resources_allowed: bool,
}

impl PromptSourceDiscovery {
    /// Creates a selector with normalized Session and global roots.
    pub(crate) fn new(
        global_root: PathBuf,
        cwd: PathBuf,
        project_resources_allowed: bool,
    ) -> Self {
        Self {
            global_root,
            cwd,
            project_resources_allowed,
        }
    }

    /// Resolves configured sources before project and global automatic candidates.
    pub(crate) fn discover(
        &self,
        policy: &PromptPolicy,
    ) -> Result<DiscoveredPromptSources, PromptError> {
        let mut diagnostics = Vec::new();
        let custom_prompt = match &policy.system_prompt {
            Some(source) => Some(source.read_from(&self.cwd)?),
            None => self.first_automatic(
                PromptSourceKind::System,
                "SYSTEM.md",
                &mut diagnostics,
            ),
        };
        let append_system_prompt = if policy.append_system_prompts.is_empty() {
            self.first_automatic(
                PromptSourceKind::AppendSystem,
                "APPEND_SYSTEM.md",
                &mut diagnostics,
            )
        } else {
            let mut content =
                Vec::with_capacity(policy.append_system_prompts.len());
            for source in &policy.append_system_prompts {
                content.push(source.read_from(&self.cwd)?);
            }
            Some(content.join("\n\n"))
        };

        Ok(DiscoveredPromptSources {
            custom_prompt,
            append_system_prompt,
            diagnostics,
        })
    }

    /// Reads the first valid project or global automatic content candidate.
    fn first_automatic(
        &self,
        kind: PromptSourceKind,
        filename: &str,
        diagnostics: &mut Vec<PromptDiagnostic>,
    ) -> Option<String> {
        let mut candidates = Vec::with_capacity(2);
        if self.project_resources_allowed {
            candidates.push((
                PromptSourceScope::Project,
                self.cwd.join(".pi").join(filename),
            ));
        }
        candidates
            .push((PromptSourceScope::User, self.global_root.join(filename)));

        for (scope, path) in candidates {
            match fs::read_to_string(&path) {
                Ok(content) => return Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => diagnostics.push(
                    PromptDiagnostic::builder()
                        .severity(PromptDiagnosticSeverity::Warning)
                        .message(format!(
                            "failed to read automatic Prompt resource '{}': {error}",
                            path.display()
                        ))
                        .source(PromptSourceInfo {
                            kind,
                            scope,
                            path: Some(path),
                        })
                        .build(),
                ),
            }
        }

        None
    }
}
