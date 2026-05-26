//! Built-in tool for reading files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::{FsBackend, FsReadRequest, LocalFsBackend, Tool};

/// Reads a file's content, optionally limited by offset and line count.
pub struct ReadFile {
    /// Backend selected when this tool was registered.
    backend: Arc<dyn FsBackend>,
}

impl ReadFile {
    /// Create a new read-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalFsBackend::new()))
    }

    /// Create a read-file tool using the provided filesystem backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn FsBackend>) -> Self {
        Self { backend }
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
        let offset = arguments
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let request = match limit {
            Some(limit) => FsReadRequest::builder()
                .session_id(ctx.session_id.clone())
                .cwd(ctx.cwd.clone())
                .path(PathBuf::from(path))
                .offset(offset)
                .limit(limit)
                .build(),
            None => FsReadRequest::builder()
                .session_id(ctx.session_id.clone())
                .cwd(ctx.cwd.clone())
                .path(PathBuf::from(path))
                .offset(offset)
                .build(),
        };

        self.backend
            .read_text_file(request)
            .await
            .map(|response| response.content)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsBackend, FsBackendError, FsReadRequest, FsReadResponse, ToolContext};
    use async_trait::async_trait;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

    /// Build a test tool context rooted at `cwd`.
    fn test_context(cwd: impl Into<std::path::PathBuf>) -> ToolContext {
        ToolContext::builder()
            .session_id(protocol::SessionId::from("test-session"))
            .cwd(cwd.into())
            .agent_path(protocol::AgentPath::root())
            .approval_mode(protocol::ApprovalMode::default())
            .build()
    }

    struct RecordingReadBackend {
        request: Mutex<Option<FsReadRequest>>,
    }

    #[async_trait]
    impl FsBackend for RecordingReadBackend {
        /// Return canned content while recording the read request.
        async fn read_text_file(
            &self,
            request: FsReadRequest,
        ) -> Result<FsReadResponse, FsBackendError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(FsReadResponse {
                content: "from backend".to_string(),
            })
        }

        /// This fake backend is only used by read-file tests.
        async fn write_text_file(
            &self,
            _request: crate::FsWriteRequest,
        ) -> Result<crate::FsWriteResponse, FsBackendError> {
            Err(FsBackendError::Io("unexpected write".to_string()))
        }
    }

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
            .execute(serde_json::json!({"path": path}), &test_context(dir.path()))
            .await
            .unwrap();

        assert!(result.contains("line2"));
    }

    /// Verifies that read-file delegates file access to the injected backend.
    #[tokio::test]
    async fn read_file_uses_injected_backend() {
        let backend = Arc::new(RecordingReadBackend {
            request: Mutex::new(None),
        });
        let tool = ReadFile::with_backend(Arc::clone(&backend) as Arc<dyn FsBackend>);

        let result = tool
            .execute(
                serde_json::json!({"path": "sample.txt", "offset": 2, "limit": 3}),
                &test_context("/workspace"),
            )
            .await
            .expect("read should use fake backend");

        assert_eq!(result, "from backend");
        let request = backend
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("read request should be recorded");
        assert_eq!(request.path, PathBuf::from("sample.txt"));
        assert_eq!(request.offset, 2);
        assert_eq!(request.limit, Some(3));
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
                &test_context(dir.path()),
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
            &test_context(Path::new("."))
        ));
    }

    /// Verifies that absolute paths request approval without local rejection.
    #[tokio::test]
    async fn read_file_requires_approval_for_absolute_path() {
        let tool = ReadFile::new();
        assert!(tool.needs_approval(
            &serde_json::json!({"path": "/etc/passwd"}),
            &test_context(Path::new("."))
        ));
        let result = tool
            .execute(
                serde_json::json!({"path": "/etc/hostname"}),
                &test_context(Path::new(".")),
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
            &test_context(Path::new("."))
        ));
    }
}
