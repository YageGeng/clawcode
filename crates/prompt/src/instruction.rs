use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use protocol::{
    ProjectInstruction, PromptDiagnostic, PromptDiagnosticSeverity,
    PromptSourceInfo, PromptSourceKind, PromptSourceScope,
};

use crate::PromptError;

/// pi-compatible instruction candidate priority for every scanned directory.
const INSTRUCTION_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// Filesystem instruction discovery for one immutable Session snapshot.
pub(crate) struct InstructionDiscovery {
    global_root: PathBuf,
    cwd: PathBuf,
    project_instructions_enabled: bool,
}

impl InstructionDiscovery {
    /// Creates discovery with a Session-specific project access decision.
    pub(crate) fn new(
        global_root: PathBuf,
        cwd: PathBuf,
        project_instructions_enabled: bool,
    ) -> Self {
        Self {
            global_root,
            cwd,
            project_instructions_enabled,
        }
    }

    /// Loads the global instruction before project ancestors ordered root to cwd.
    pub(crate) fn discover(
        &self,
    ) -> Result<(Vec<ProjectInstruction>, Vec<PromptDiagnostic>), PromptError>
    {
        let mut instructions = Vec::new();
        let mut diagnostics = Vec::new();
        let mut seen_paths = BTreeSet::new();

        if let Some(instruction) = self.load_from_directory(
            &self.global_root,
            PromptSourceScope::User,
            &mut diagnostics,
        ) {
            seen_paths.insert(instruction.path.clone());
            instructions.push(instruction);
        }
        if !self.project_instructions_enabled {
            return Ok((instructions, diagnostics));
        }

        let cwd = fs::canonicalize(&self.cwd).map_err(|error| {
            PromptError::ConfiguredResource {
                path: self.cwd.clone(),
                reason: error.to_string(),
            }
        })?;
        let shadowed = self.shadowed_instruction_path(&cwd);
        let mut ancestors: Vec<_> =
            cwd.ancestors().map(Path::to_path_buf).collect();
        ancestors.reverse();
        for ancestor in ancestors {
            let Some(instruction) = self.load_from_directory(
                &ancestor,
                PromptSourceScope::Project,
                &mut diagnostics,
            ) else {
                continue;
            };
            if shadowed.as_ref() == Some(&instruction.path)
                || !seen_paths.insert(instruction.path.clone())
            {
                continue;
            }
            instructions.push(instruction);
        }

        Ok((instructions, diagnostics))
    }

    /// Loads the first readable regular instruction candidate from one directory.
    fn load_from_directory(
        &self,
        directory: &Path,
        scope: PromptSourceScope,
        diagnostics: &mut Vec<PromptDiagnostic>,
    ) -> Option<ProjectInstruction> {
        for filename in INSTRUCTION_CANDIDATES {
            let path = directory.join(filename);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => {
                    diagnostics.push(Self::warning(
                        path,
                        scope,
                        format!("failed to inspect instruction file: {error}"),
                    ));
                    continue;
                }
            };
            if !metadata.is_file() {
                continue;
            }
            let canonical_path = match fs::canonicalize(&path) {
                Ok(path) => path,
                Err(error) => {
                    diagnostics.push(Self::warning(
                        path,
                        scope,
                        format!(
                            "failed to normalize instruction file: {error}"
                        ),
                    ));
                    continue;
                }
            };
            match fs::read_to_string(&canonical_path) {
                Ok(content) => {
                    return Some(ProjectInstruction {
                        path: canonical_path.clone(),
                        content,
                        source: PromptSourceInfo {
                            kind: PromptSourceKind::Instruction,
                            scope,
                            path: Some(canonical_path),
                        },
                    });
                }
                Err(error) => diagnostics.push(Self::warning(
                    canonical_path,
                    scope,
                    format!("failed to read instruction file: {error}"),
                )),
            }
        }

        None
    }

    /// Resolves the main-worktree instruction shadowed by a nested linked worktree.
    fn shadowed_instruction_path(&self, cwd: &Path) -> Option<PathBuf> {
        let mut repository_root = None;
        let mut common_git_directory = None;

        for ancestor in cwd.ancestors() {
            let dot_git = ancestor.join(".git");
            let Ok(metadata) = fs::metadata(&dot_git) else {
                continue;
            };
            if metadata.is_dir() {
                repository_root = Some(ancestor.to_path_buf());
                common_git_directory = fs::canonicalize(dot_git).ok();
                break;
            }
            if !metadata.is_file() {
                continue;
            }
            let declaration = fs::read_to_string(&dot_git).ok()?;
            let git_directory = declaration.trim().strip_prefix("gitdir: ")?;
            let git_directory = PathBuf::from(git_directory);
            let git_directory = if git_directory.is_absolute() {
                git_directory
            } else {
                ancestor.join(git_directory)
            };
            let git_directory = fs::canonicalize(git_directory).ok()?;
            if !git_directory.join("HEAD").is_file() {
                return None;
            }
            let common_directory_file = git_directory.join("commondir");
            let common_directory = if common_directory_file.is_file() {
                let relative =
                    fs::read_to_string(common_directory_file).ok()?;
                fs::canonicalize(git_directory.join(relative.trim())).ok()?
            } else {
                git_directory
            };
            repository_root = Some(ancestor.to_path_buf());
            common_git_directory = Some(common_directory);
            break;
        }

        let repository_root = fs::canonicalize(repository_root?).ok()?;
        let common_git_directory = common_git_directory?;
        let main_repository_root = common_git_directory.parent()?;
        if repository_root == main_repository_root
            || !repository_root.starts_with(main_repository_root)
            || fs::canonicalize(main_repository_root.join(".git")).ok()?
                != common_git_directory
        {
            return None;
        }

        for filename in INSTRUCTION_CANDIDATES {
            if repository_root.join(filename).is_file() {
                return fs::canonicalize(main_repository_root.join(filename))
                    .ok();
            }
        }

        None
    }

    /// Builds one non-fatal instruction discovery warning.
    fn warning(
        path: PathBuf,
        scope: PromptSourceScope,
        message: String,
    ) -> PromptDiagnostic {
        PromptDiagnostic::builder()
            .severity(PromptDiagnosticSeverity::Warning)
            .message(message)
            .source(PromptSourceInfo {
                kind: PromptSourceKind::Instruction,
                scope,
                path: Some(path),
            })
            .build()
    }
}
