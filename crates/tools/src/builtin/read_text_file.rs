use std::{
    fs,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde::Deserialize;
use snafu::{ResultExt, ensure};

use crate::{
    Result,
    context::{ToolInvocation, ToolOutput},
    error::{ToolExecutionSnafu, ToolIoSnafu},
    handler::ToolHandler,
};

/// Parses arguments for the built-in `fs/read_text_file` tool.
#[derive(Debug, Deserialize)]
struct ReadTextFileArgs {
    path: PathBuf,
    line: Option<u32>,
    limit: Option<u32>,
}

/// Reads UTF-8 text files while enforcing a workspace-root filesystem boundary.
pub struct ReadTextFileTool {
    root_dir: PathBuf,
}

impl ReadTextFileTool {
    /// Creates a read-text-file tool rooted at the provided workspace directory.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
        }
    }

    /// Reads one text file under this tool's root and applies optional line slicing.
    pub fn read_text_file(
        &self,
        path: PathBuf,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<ToolOutput> {
        let resolved = self.resolve_read_path(&path)?;
        let content = fs::read_to_string(&resolved).context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "read-text-file-read".to_string(),
        })?;
        let content = slice_text_lines(&content, line, limit);

        Ok(ToolOutput {
            text: content.clone(),
            structured: serde_json::json!({ "content": content }),
        })
    }

    /// Resolves a requested read path and rejects paths outside the configured root.
    fn resolve_read_path(&self, requested: &Path) -> Result<PathBuf> {
        ensure!(
            !requested.as_os_str().is_empty(),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: "read-text-file-path".to_string(),
                message: "path must not be empty".to_string(),
            }
        );

        let canonical_root = self.root_dir.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "read-text-file-root-canonicalize".to_string(),
        })?;
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            canonical_root.join(requested)
        };
        let canonical_path = candidate.canonicalize().context(ToolIoSnafu {
            tool: self.name().to_string(),
            stage: "read-text-file-path-canonicalize".to_string(),
        })?;

        ensure!(
            canonical_path.starts_with(&canonical_root),
            ToolExecutionSnafu {
                tool: self.name().to_string(),
                stage: "read-text-file-path".to_string(),
                message: "path must stay inside the tool root".to_string(),
            }
        );

        Ok(canonical_path)
    }
}

#[async_trait]
impl ToolHandler for ReadTextFileTool {
    fn name(&self) -> &'static str {
        "fs/read_text_file"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file under the configured workspace root."
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Read UTF-8 text files from the workspace.".to_string())
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to read, relative to the workspace root or absolute inside it."
                },
                "line": {
                    "type": "integer",
                    "description": "Optional 1-based starting line."
                },
                "limit": {
                    "type": "integer",
                    "description": "Optional maximum number of lines."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    /// Parses read-file arguments and returns the requested text content.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: ReadTextFileArgs =
            invocation.parse_function_arguments("read-text-file-parse-args")?;
        self.read_text_file(args.path, args.line, args.limit)
    }
}

/// Applies ACP-compatible 1-based `line` and maximum `limit` parameters to text content.
fn slice_text_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_string();
    }

    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let mut lines = content.split_inclusive('\n').skip(start);
    match limit {
        Some(limit) => lines.by_ref().take(limit as usize).collect(),
        None => lines.collect(),
    }
}
