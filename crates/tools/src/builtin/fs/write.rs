//! Built-in tool for writing files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;

use crate::Tool;

/// Creates or overwrites a file with the given content.
pub struct WriteFile;

impl WriteFile {
    /// Create a new write-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let path = arguments["path"]
            .as_str()
            .ok_or("missing 'path' argument")?;
        let content = arguments["content"]
            .as_str()
            .ok_or("missing 'content' argument")?;

        // Approval is handled by the caller before execute reaches disk mutation.
        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            ctx.cwd.join(path)
        };

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("failed to create parent dir: {e}"))?;
        }

        fs::write(&resolved, content)
            .await
            .map_err(|e| format!("failed to write {}: {e}", resolved.display()))?;

        Ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            resolved.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use crate::builtin::fs::read::ReadFile;

    /// Verifies that written content can be read back.
    #[tokio::test]
    async fn write_and_read_file() {
        let dir = tempfile::tempdir().unwrap();

        let write = WriteFile::new();
        write
            .execute(
                serde_json::json!({"path": "test.txt", "content": "hello world"}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        let read = ReadFile::new();
        let result = read
            .execute(
                serde_json::json!({"path": "test.txt"}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(result, "hello world");
    }

    /// Verifies that relative writes stay rooted under cwd.
    #[tokio::test]
    async fn write_file_resolves_relative_path_under_cwd() {
        let dir = tempfile::tempdir().unwrap();

        let tool = WriteFile::new();
        tool.execute(
            serde_json::json!({"path": "nested/test.txt", "content": "cwd scoped"}),
            &ToolContext::for_test(dir.path()),
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("nested/test.txt")).unwrap();
        assert_eq!(content, "cwd scoped");
    }

    /// Verifies that write operations always request approval.
    #[tokio::test]
    async fn write_file_always_requires_approval() {
        let tool = WriteFile::new();
        assert!(tool.needs_approval(&serde_json::json!({"path": "test.txt"})));
        assert!(tool.needs_approval(&serde_json::json!({"path": "../escape.txt"})));
    }
}
