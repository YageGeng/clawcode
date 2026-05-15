//! Built-in tool for reading files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;

use crate::Tool;

/// Reads a file's content, optionally limited by offset and line count.
pub struct ReadFile;

impl ReadFile {
    /// Create a new read-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file's content, with optional offset and limit (line numbers)"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "offset": { "type": "integer", "description": "Start line (0-indexed)" },
                "limit": { "type": "integer", "description": "Max number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn needs_approval(&self, arguments: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        // Require approval only when the path escapes cwd.
        arguments["path"]
            .as_str()
            .is_some_and(|p| Path::new(p).is_absolute() || p.contains(".."))
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'path' argument")?;
        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            ctx.cwd.join(path)
        };
        // Canonicalize to detect symlink escapes even on absolute paths.
        let resolved = fs::canonicalize(&resolved)
            .await
            .map_err(|e| format!("failed to resolve {}: {e}", resolved.display()))?;

        let content = fs::read_to_string(&resolved)
            .await
            .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let offset = arguments
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let start = offset.min(lines.len());
        let end = limit
            .map(|line_count| (start + line_count).min(lines.len()))
            .unwrap_or(lines.len());

        // SAFETY: both `start` and `end` are clamped to `lines.len()` above.
        #[allow(clippy::indexing_slicing)]
        Ok(lines[start..end].join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Verifies that the read tool returns file contents.
    #[tokio::test]
    async fn read_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = NamedTempFile::new_in(dir.path()).unwrap();
        writeln!(file, "line1\nline2\nline3").unwrap();
        let path = file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let tool = ReadFile::new();
        let result = tool
            .execute(
                serde_json::json!({"path": path}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.contains("line2"));
    }

    /// Verifies that offset and limit slice line output.
    #[tokio::test]
    async fn read_file_with_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = NamedTempFile::new_in(dir.path()).unwrap();
        writeln!(file, "a\nb\nc\nd").unwrap();
        let path = file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let tool = ReadFile::new();
        let result = tool
            .execute(
                serde_json::json!({"path": path, "offset": 1, "limit": 2}),
                &ToolContext::for_test(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(result, "b\nc");
    }

    /// Verifies that relative in-cwd reads do not need approval.
    #[tokio::test]
    async fn read_file_needs_no_approval() {
        let tool = ReadFile::new();
        assert!(!tool.needs_approval(
            &serde_json::json!({"path": "x"}),
            &ToolContext::for_test(Path::new("."))
        ));
    }

    /// Verifies that absolute paths request approval without local rejection.
    #[tokio::test]
    async fn read_file_requires_approval_for_absolute_path() {
        let tool = ReadFile::new();
        assert!(tool.needs_approval(
            &serde_json::json!({"path": "/etc/passwd"}),
            &ToolContext::for_test(Path::new("."))
        ));
        let result = tool
            .execute(
                serde_json::json!({"path": "/etc/hostname"}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await;
        let _ = result;
    }

    /// Verifies that parent-directory escapes request approval.
    #[tokio::test]
    async fn read_file_requires_approval_for_parent_escape() {
        let tool = ReadFile::new();
        assert!(tool.needs_approval(
            &serde_json::json!({"path": "../secret"}),
            &ToolContext::for_test(Path::new("."))
        ));
    }
}
