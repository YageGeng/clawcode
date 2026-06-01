//! Hashline write-file tool.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{FsBackend, FsWriteRequest, LocalFsBackend, Tool};

/// Creates or overwrites a file while registered as part of the hashline tool set.
pub struct HashlineWriteFile {
    /// Backend selected when this tool was registered.
    backend: Arc<dyn FsBackend>,
}

impl HashlineWriteFile {
    /// Create a new hashline write-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalFsBackend::new()))
    }

    /// Create a hashline write-file tool using the provided filesystem backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn FsBackend>) -> Self {
        Self { backend }
    }
}

impl Default for HashlineWriteFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for HashlineWriteFile {
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

        // Approval is handled before the tool reaches this disk mutation.
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
    use crate::{
        FsBackendError, FsReadRequest, FsReadResponse, FsWriteResponse,
        ToolContext,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Build a test tool context rooted at `cwd`.
    fn test_context(cwd: impl Into<std::path::PathBuf>) -> ToolContext {
        ToolContext::builder()
            .session_id(protocol::SessionId::from("test-session"))
            .cwd(cwd.into())
            .agent_path(protocol::AgentPath::root())
            .approval_mode(protocol::ApprovalMode::default())
            .build()
    }

    struct RecordingWriteBackend {
        request: Mutex<Option<FsWriteRequest>>,
    }

    #[async_trait]
    impl FsBackend for RecordingWriteBackend {
        /// This fake backend is only used by hashline write tests.
        async fn read_text_file(
            &self,
            _request: FsReadRequest,
        ) -> Result<FsReadResponse, FsBackendError> {
            Err(FsBackendError::Io("unexpected read".to_string()))
        }

        /// Return canned write metadata while recording the request.
        async fn write_text_file(
            &self,
            request: FsWriteRequest,
        ) -> Result<FsWriteResponse, FsBackendError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(request);
            Ok(FsWriteResponse {
                bytes_written: 12,
                display_path: std::path::PathBuf::from("/workspace/out.txt"),
            })
        }
    }

    /// Verifies that hashline write delegates file access to the injected backend.
    #[tokio::test]
    async fn hashline_write_file_uses_injected_backend() {
        let backend = Arc::new(RecordingWriteBackend {
            request: Mutex::new(None),
        });
        let tool = HashlineWriteFile::with_backend(
            Arc::clone(&backend) as Arc<dyn FsBackend>
        );

        let result = tool
            .execute(
                serde_json::json!({"path": "out.txt", "content": "hello world!"}),
                &test_context("/workspace"),
            )
            .await
            .expect("write should succeed");

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
}
