//! Built-in tool for writing files.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{FsBackend, FsWriteRequest, LocalFsBackend, Tool};

/// Creates or overwrites a file with the given content.
pub struct WriteFile {
    /// Backend selected when this tool was registered.
    backend: Arc<dyn FsBackend>,
}

impl WriteFile {
    /// Create a new write-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalFsBackend::new()))
    }

    /// Create a write-file tool using the provided filesystem backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn FsBackend>) -> Self {
        Self { backend }
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

    fn needs_approval(&self, _: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        true
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
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("missing 'content' argument")?;

        // Approval is handled by the caller before execute reaches disk mutation.
        let response = self
            .backend
            .write_text_file(
                FsWriteRequest::builder()
                    .session_id(ctx.session_id.clone())
                    .cwd(ctx.cwd.clone())
                    .path(std::path::PathBuf::from(path))
                    .content(content.to_string())
                    .build(),
            )
            .await
            .map_err(|error| error.to_string())?;

        Ok(format!(
            "wrote {} bytes to {}",
            response.bytes_written,
            response.display_path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use crate::builtin::fs::read::ReadFile;
    use crate::{FsBackend, FsBackendError, FsReadRequest, FsReadResponse, FsWriteResponse};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct RecordingWriteBackend {
        request: Mutex<Option<crate::FsWriteRequest>>,
    }

    #[async_trait]
    impl FsBackend for RecordingWriteBackend {
        /// This fake backend is only used by write-file tests.
        async fn read_text_file(
            &self,
            _request: FsReadRequest,
        ) -> Result<FsReadResponse, FsBackendError> {
            Err(FsBackendError::Io("unexpected read".to_string()))
        }

        /// Return canned write metadata while recording the write request.
        async fn write_text_file(
            &self,
            request: crate::FsWriteRequest,
        ) -> Result<FsWriteResponse, FsBackendError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(FsWriteResponse {
                bytes_written: 12,
                display_path: std::path::PathBuf::from("/workspace/out.txt"),
            })
        }
    }

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

    /// Verifies that write-file delegates file access to the injected backend.
    #[tokio::test]
    async fn write_file_uses_injected_backend() {
        let backend = Arc::new(RecordingWriteBackend {
            request: Mutex::new(None),
        });
        let tool = WriteFile::with_backend(Arc::clone(&backend) as Arc<dyn FsBackend>);

        let result = tool
            .execute(
                serde_json::json!({"path": "out.txt", "content": "hello world!"}),
                &ToolContext::for_test("/workspace"),
            )
            .await
            .expect("write should use fake backend");

        assert_eq!(result, "wrote 12 bytes to /workspace/out.txt");
        let request = backend
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("write request should be recorded");
        assert_eq!(request.path, std::path::PathBuf::from("out.txt"));
        assert_eq!(request.content, "hello world!");
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
        assert!(tool.needs_approval(
            &serde_json::json!({"path": "test.txt"}),
            &ToolContext::for_test(Path::new("."))
        ));
        assert!(tool.needs_approval(
            &serde_json::json!({"path": "../escape.txt"}),
            &ToolContext::for_test(Path::new("."))
        ));
    }
}
