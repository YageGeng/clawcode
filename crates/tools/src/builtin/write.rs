use async_trait::async_trait;
use protocol::{ContentBlock, ToolCall, ToolDefinition, ToolResult};
use serde::Deserialize;

use super::mutation::with_file_mutation;
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

    /// Validates write JSON without creating directories or changing files.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        WriteArguments::try_from(call).map(|_arguments| ())
    }

    /// Creates parent directories and writes the complete content under a per-file queue.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let arguments = WriteArguments::try_from(&call)?;
        let absolute = ResolvedPath::new(&arguments.path, &context.cwd)?;
        with_file_mutation(absolute.as_path(), || async {
            context.ensure_active()?;
            if let Some(parent) = absolute.as_path().parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    ToolError::Execution {
                        tool: call.name.clone(),
                        message: error.to_string(),
                    }
                })?;
            }
            context.ensure_active()?;
            tokio::fs::write(absolute.as_path(), arguments.content.as_bytes())
                .await
                .map_err(|error| ToolError::Execution {
                    tool: call.name.clone(),
                    message: error.to_string(),
                })?;
            context.ensure_active()?;
            Ok(ToolResult::builder()
                .tool_call_id(call.tool_call_id)
                .blocks(vec![ContentBlock::Text {
                    text: format!(
                        "Successfully wrote {} bytes to {}",
                        arguments.content.encode_utf16().count(),
                        arguments.path
                    ),
                }])
                .is_error(false)
                .build())
        })
        .await
    }
}
