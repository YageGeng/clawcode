use async_trait::async_trait;
use protocol::{
    ContentBlock, ToolCall, ToolDefinition, ToolPromptContribution, ToolResult,
};
use serde::Deserialize;

use super::mutation::FileMutation;
use super::path::ResolvedPath;
use crate::{AgentTool, ToolError, ToolExecutionContext};

#[derive(Debug, Deserialize)]
struct WriteArguments {
    path: String,
    content: String,
}

impl TryFrom<&ToolCall> for WriteArguments {
    type Error = ToolError;

    /// Decodes one write call without consuming its execution correlation.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        serde_json::from_value(call.arguments.clone()).map_err(|error| {
            ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: error.to_string(),
            }
        })
    }
}

/// Pi-compatible local file creator and complete-file writer.
pub(super) struct WriteTool;

#[async_trait]
impl AgentTool for WriteTool {
    /// Returns pi's write schema and overwrite semantics.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    /// Contributes pi's complete-file writing capability and usage guideline.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: Some("Create or overwrite files".to_string()),
            guidelines: vec![
                "Use write only for new files or complete rewrites."
                    .to_string(),
            ],
        }
    }

    /// Validates write JSON without creating directories or changing files.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        WriteArguments::try_from(call).map(|_arguments| ())
    }

    /// Reports the file a write creates or replaces.
    async fn locations(
        &self,
        call: &ToolCall,
        cwd: &std::path::Path,
    ) -> Vec<protocol::ToolLocation> {
        WriteArguments::try_from(call)
            .ok()
            .and_then(|arguments| ResolvedPath::new(&arguments.path, cwd).ok())
            .map(|path| protocol::ToolLocation {
                path: path.as_path().to_path_buf(),
                line: None,
            })
            .into_iter()
            .collect()
    }

    /// Creates parent directories and writes the complete content under a per-file queue.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let arguments = WriteArguments::try_from(&call)?;
        let target =
            FileMutation::prepare(&arguments.path, &call, context).await?;
        let _guard = target.lock().await;
        context.ensure_active()?;
        let written = arguments.content.encode_utf16().count();
        target
            .save(None, arguments.content)
            .await
            .map_err(|error| {
                tracing::warn!(
                    "could not save write target {}: {}",
                    target.path.display(),
                    error
                );
                ToolError::Execution {
                    tool: call.name.clone(),
                    message: error.to_string(),
                }
            })?;
        context.ensure_active()?;
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: format!(
                    "Successfully wrote {} bytes to {}",
                    written, arguments.path
                ),
            }])
            .is_error(false)
            .build())
    }
}
