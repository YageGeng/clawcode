use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use async_trait::async_trait;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    ApprovalRequirement, Result, RiskLevel,
    context::{ToolInvocation, ToolMetadata, ToolOutput},
    error::{ToolExecutionSnafu, ToolIoSnafu},
    handler::ToolHandler,
};

/// Parses arguments for the built-in `apply_patch` tool.
#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
}

/// Applies OpenAI-style patch streams directly against the local workspace root.
pub struct ApplyPatchTool {
    root_dir: PathBuf,
}

impl ApplyPatchTool {
    /// Creates an apply-patch tool rooted at the provided workspace directory.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    /// Resolves a patch path under the configured root and rejects traversal.
    fn resolve_path(&self, raw_path: &str, stage: &'static str) -> Result<PathBuf> {
        ensure!(
            !raw_path.trim().is_empty(),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: "path must not be empty".to_string(),
            }
        );

        let requested = Path::new(raw_path);
        ensure!(
            !requested.is_absolute(),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: "absolute paths are not allowed".to_string(),
            }
        );
        ensure!(
            !requested
                .components()
                .any(|component| matches!(component, Component::ParentDir)),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: "path traversal via `..` is not allowed".to_string(),
            }
        );

        let canonical_root = self.root_dir.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: format!("{stage}-root-canonicalize"),
        })?;
        let candidate = canonical_root.join(requested);
        ensure!(
            candidate.starts_with(&canonical_root),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: stage.to_string(),
                message: "target must stay inside the tool root".to_string(),
            }
        );
        Ok(candidate)
    }

    /// Applies one parsed patch action to disk.
    fn apply_action(&self, action: PatchAction) -> Result<String> {
        match action {
            PatchAction::Add { path, lines } => {
                let resolved = self.resolve_path(&path, "apply-patch-add-path")?;
                if let Some(parent) = resolved.parent() {
                    fs::create_dir_all(parent).context(ToolIoSnafu {
                        tool: self.name().to_string(),
                        stage: "apply-patch-add-parent".to_string(),
                    })?;
                }

                let content = join_patch_lines(&lines);
                fs::write(&resolved, content).context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "apply-patch-add-write".to_string(),
                })?;
                Ok(format!("added {}", resolved.to_string_lossy()))
            }
            PatchAction::Delete { path } => {
                let resolved = self.resolve_path(&path, "apply-patch-delete-path")?;
                fs::remove_file(&resolved).context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "apply-patch-delete-file".to_string(),
                })?;
                Ok(format!("deleted {}", resolved.to_string_lossy()))
            }
            PatchAction::Update {
                path,
                move_to,
                hunks,
            } => {
                let resolved = self.resolve_path(&path, "apply-patch-update-path")?;
                let original = fs::read_to_string(&resolved).context(ToolIoSnafu {
                    tool: self.name().to_string(),
                    stage: "apply-patch-update-read".to_string(),
                })?;
                let updated = apply_hunks(self.name(), &path, &original, &hunks)?;
                if let Some(target_path) = move_to {
                    // Apply the edit to the destination path first so rename patches can also
                    // create missing parent directories under the guarded workspace root.
                    let moved = self.resolve_path(&target_path, "apply-patch-move-path")?;
                    // Treat self-renames as plain updates so a no-op `Move to` header cannot
                    // delete the source file after writing the updated content back in place.
                    if moved == resolved {
                        fs::write(&resolved, updated).context(ToolIoSnafu {
                            tool: self.name().to_string(),
                            stage: "apply-patch-update-write".to_string(),
                        })?;
                        return Ok(format!("updated {}", resolved.to_string_lossy()));
                    }
                    if let Some(parent) = moved.parent() {
                        fs::create_dir_all(parent).context(ToolIoSnafu {
                            tool: self.name().to_string(),
                            stage: "apply-patch-move-parent".to_string(),
                        })?;
                    }
                    fs::write(&moved, updated).context(ToolIoSnafu {
                        tool: self.name().to_string(),
                        stage: "apply-patch-move-write".to_string(),
                    })?;
                    fs::remove_file(&resolved).context(ToolIoSnafu {
                        tool: self.name().to_string(),
                        stage: "apply-patch-move-remove-source".to_string(),
                    })?;
                    Ok(format!(
                        "moved {} to {}",
                        resolved.to_string_lossy(),
                        moved.to_string_lossy()
                    ))
                } else {
                    fs::write(&resolved, updated).context(ToolIoSnafu {
                        tool: self.name().to_string(),
                        stage: "apply-patch-update-write".to_string(),
                    })?;
                    Ok(format!("updated {}", resolved.to_string_lossy()))
                }
            }
        }
    }

    /// Parses patch text and applies it relative to this tool's configured root.
    pub(crate) fn apply_patch_text(&self, patch: &str) -> Result<ToolOutput> {
        let actions = parse_patch(patch, self.name())?;
        let mut applied = Vec::with_capacity(actions.len());
        for action in actions {
            applied.push(self.apply_action(action)?);
        }

        Ok(ToolOutput {
            text: applied.join("\n"),
            structured: serde_json::json!({
                "status": "ok",
                "changes": applied,
            }),
        })
    }
}

#[async_trait]
impl ToolHandler for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Apply an OpenAI-style patch stream to files under the configured workspace root."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Patch text in the `*** Begin Patch` format."
                }
            },
            "required": ["patch"]
        })
    }

    /// Marks the patch tool as high risk because it can mutate multiple files at once.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Parses a patch stream and applies each action sequentially.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: ApplyPatchArgs = invocation.parse_function_arguments("apply-patch-parse-args")?;
        self.apply_patch_text(&args.patch)
    }
}

/// Represents one parsed apply-patch action.
enum PatchAction {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<PatchHunk>,
    },
}

/// Stores the lines belonging to a single update hunk.
struct PatchHunk {
    lines: Vec<PatchLine>,
}

/// Represents one line inside an update hunk.
enum PatchLine {
    Context(String),
    Delete(String),
    Add(String),
}

/// Parses a full patch stream into discrete actions.
fn parse_patch(patch: &str, tool: &'static str) -> Result<Vec<PatchAction>> {
    let lines = patch.lines().collect::<Vec<_>>();
    ensure!(
        lines.first() == Some(&"*** Begin Patch"),
        ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: "apply-patch-parse".to_string(),
            message: "patch must start with `*** Begin Patch`".to_string(),
        }
    );
    ensure!(
        lines.last() == Some(&"*** End Patch"),
        ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: "apply-patch-parse".to_string(),
            message: "patch must end with `*** End Patch`".to_string(),
        }
    );

    let mut actions = Vec::new();
    let mut index = 1usize;
    while index < lines.len() - 1 {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut body = Vec::new();
            while index < lines.len() - 1 && !lines[index].starts_with("*** ") {
                let raw = lines[index];
                let added = raw.strip_prefix('+').context(ToolExecutionSnafu {
                    tool: tool.to_string(),
                    stage: "apply-patch-parse-add".to_string(),
                    message: "add-file bodies must use `+` lines".to_string(),
                })?;
                body.push(added.to_string());
                index += 1;
            }
            actions.push(PatchAction::Add {
                path: path.to_string(),
                lines: body,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            actions.push(PatchAction::Delete {
                path: path.to_string(),
            });
            index += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut move_to = None;
            if index < lines.len() - 1
                && let Some(target) = lines[index].strip_prefix("*** Move to: ")
            {
                move_to = Some(target.to_string());
                index += 1;
            }
            let mut hunks = Vec::new();
            while index < lines.len() - 1 && !lines[index].starts_with("*** ") {
                ensure!(
                    lines[index].starts_with("@@"),
                    ToolExecutionSnafu {
                        tool: tool.to_string(),
                        stage: "apply-patch-parse-update".to_string(),
                        message: "update blocks must start with `@@`".to_string(),
                    }
                );
                index += 1;
                let mut hunk_lines = Vec::new();
                while index < lines.len() - 1
                    && !lines[index].starts_with("@@")
                    && !lines[index].starts_with("*** ")
                {
                    let raw = lines[index];
                    if raw == "*** End of File" {
                        index += 1;
                        break;
                    }
                    let (prefix, content) = raw.split_at(1);
                    let patch_line = match prefix {
                        " " => PatchLine::Context(content.to_string()),
                        "-" => PatchLine::Delete(content.to_string()),
                        "+" => PatchLine::Add(content.to_string()),
                        _ => {
                            return ToolExecutionSnafu {
                                tool: tool.to_string(),
                                stage: "apply-patch-parse-update".to_string(),
                                message: format!("invalid update line: {raw}"),
                            }
                            .fail();
                        }
                    };
                    hunk_lines.push(patch_line);
                    index += 1;
                }
                hunks.push(PatchHunk { lines: hunk_lines });
            }
            actions.push(PatchAction::Update {
                path: path.to_string(),
                move_to,
                hunks,
            });
            continue;
        }

        return ToolExecutionSnafu {
            tool: tool.to_string(),
            stage: "apply-patch-parse".to_string(),
            message: format!("unsupported patch header: {line}"),
        }
        .fail();
    }

    Ok(actions)
}

/// Applies parsed hunks against an original file body.
fn apply_hunks(
    tool: &'static str,
    path: &str,
    original: &str,
    hunks: &[PatchHunk],
) -> Result<String> {
    let old_lines = original
        .lines()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut cursor = 0usize;

    for hunk in hunks {
        let old_pattern = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Delete(text) => Some(text.clone()),
                PatchLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let new_pattern = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Add(text) => Some(text.clone()),
                PatchLine::Delete(_) => None,
            })
            .collect::<Vec<_>>();

        let position = find_subsequence(&old_lines[cursor..], &old_pattern)
            .map(|offset| cursor + offset)
            .context(ToolExecutionSnafu {
                tool: tool.to_string(),
                stage: "apply-patch-update-match".to_string(),
                message: format!("failed to match update hunk for `{path}`"),
            })?;
        output.extend_from_slice(&old_lines[cursor..position]);
        output.extend(new_pattern);
        cursor = position + old_pattern.len();
    }

    output.extend_from_slice(&old_lines[cursor..]);
    let mut rendered = output.join("\n");
    if original.ends_with('\n') || !rendered.is_empty() {
        rendered.push('\n');
    }
    Ok(rendered)
}

/// Finds the first occurrence of a line subsequence.
fn find_subsequence(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Joins added lines into a final file body while preserving a trailing newline.
fn join_patch_lines(lines: &[String]) -> String {
    let mut rendered = lines.join("\n");
    if !rendered.is_empty() {
        rendered.push('\n');
    }
    rendered
}
