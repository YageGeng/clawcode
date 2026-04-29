use std::{
    fs,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    ApprovalRequirement, Result, RiskLevel,
    context::{
        StructuredToolOutput, ToolInvocation, ToolMetadata, ToolOutput,
        WriteTextFileStructuredOutput,
    },
    error::{ToolExecutionSnafu, ToolIoSnafu},
    handler::ToolHandler,
};

/// Parses arguments for the built-in `fs/write_text_file` tool.
#[derive(Debug, Deserialize)]
struct WriteTextFileArgs {
    path: PathBuf,
    content: String,
}

/// Writes UTF-8 text files while enforcing a workspace-root filesystem boundary.
pub struct WriteTextFileTool {
    root_dir: PathBuf,
}

impl WriteTextFileTool {
    /// Creates a write-text-file tool rooted at the provided workspace directory.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    /// Writes one text file under this tool's root.
    pub fn write_text_file(&self, path: PathBuf, content: String) -> Result<ToolOutput> {
        let resolved = self.resolve_write_path(&path)?;
        fs::write(&resolved, content).context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "write-text-file-write".to_string(),
        })?;

        Ok(ToolOutput {
            text: "file written".to_string(),
            structured: StructuredToolOutput::WriteTextFile(WriteTextFileStructuredOutput {
                ok: true,
            }),
        })
    }

    /// Resolves a write path and rejects existing targets or parents outside the configured root.
    fn resolve_write_path(&self, requested: &Path) -> Result<PathBuf> {
        ensure!(
            !requested.as_os_str().is_empty(),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: "write-text-file-path".to_string(),
                message: "path must not be empty".to_string(),
            }
        );

        let canonical_root = self.root_dir.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "write-text-file-root-canonicalize".to_string(),
        })?;
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            canonical_root.join(requested)
        };

        if candidate.exists() {
            let canonical_path = candidate.canonicalize().context(ToolIoSnafu {
                tool: self.name().to_string(),
                stage: "write-text-file-path-canonicalize".to_string(),
            })?;
            ensure!(
                canonical_path.starts_with(&canonical_root),
                ToolExecutionSnafu {
                    tool: self.name().to_string(),
                    stage: "write-text-file-path".to_string(),
                    message: "path must stay inside the tool root".to_string(),
                }
            );
            return Ok(canonical_path);
        }

        let parent = candidate.parent().context(ToolExecutionSnafu {
            tool: self.name().to_string(),
            stage: "write-text-file-parent".to_string(),
            message: "path requires a parent".to_string(),
        })?;
        let canonical_parent = parent.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "write-text-file-parent-canonicalize".to_string(),
        })?;
        ensure!(
            canonical_parent.starts_with(&canonical_root),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: "write-text-file-parent".to_string(),
                message: "path must stay inside the tool root".to_string(),
            }
        );

        Ok(
            canonical_parent.join(candidate.file_name().context(ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: "write-text-file-name".to_string(),
                message: "path requires a file name".to_string(),
            })?),
        )
    }
}

#[async_trait]
impl ToolHandler for WriteTextFileTool {
    fn name(&self) -> &'static str {
        "fs/write_text_file"
    }

    fn description(&self) -> &'static str {
        "Write a UTF-8 text file under the configured workspace root."
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Write complete UTF-8 text files inside the workspace.".to_string())
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to write, relative to the workspace root or absolute inside it."
                },
                "content": {
                    "type": "string",
                    "description": "Complete file content."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    /// Marks the write tool as high risk because it mutates workspace files.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            risk_level: RiskLevel::High,
            approval: ApprovalRequirement::Always,
            timeout: std::time::Duration::from_secs(10),
        }
    }

    /// Parses write-file arguments and writes the requested content.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: WriteTextFileArgs =
            invocation.parse_function_arguments("write-text-file-parse-args")?;
        self.write_text_file(args.path, args.content)
    }
}
