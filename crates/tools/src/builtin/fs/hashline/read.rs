//! Hashline read-file tool.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::{FsBackend, FsReadRequest, LocalFsBackend, Tool};

use super::format::format_hash_lines;

const DEFAULT_MAX_LINES: usize = 2000;

/// Reads files with `LINE:HASH|content` prefixes for hashline editing.
pub struct HashlineReadFile {
    /// Backend selected when this tool was registered.
    backend: Arc<dyn FsBackend>,
}

impl HashlineReadFile {
    /// Create a new hashline read-file tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backend(Arc::new(LocalFsBackend::new()))
    }

    /// Create a hashline read-file tool using the provided backend.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn FsBackend>) -> Self {
        Self { backend }
    }
}

impl Default for HashlineReadFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for HashlineReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file with hashline-prefixed output for verified line edits"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "offset": { "type": "integer", "description": "One-indexed line number to start reading" },
                "limit": { "type": "integer", "description": "Maximum number of lines to read" },
                "plain": { "type": "boolean", "description": "Return LINE|content without hashes" }
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
        let start_line = arguments
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value.max(1) as usize)
            .unwrap_or(1);
        let limit = arguments
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_LINES);
        let plain = arguments
            .get("plain")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let response = self
            .backend
            .read_text_file(
                FsReadRequest::builder()
                    .session_id(ctx.session_id.clone())
                    .cwd(ctx.cwd.clone())
                    .path(PathBuf::from(path))
                    .offset(0)
                    .preserve_full(true)
                    .build(),
            )
            .await
            .map_err(|error| error.to_string())?;

        Ok(format_read_response(
            path,
            &response.content,
            start_line,
            limit,
            plain,
        ))
    }
}

/// Format a hashline read response with header metadata and a fenced body.
#[must_use]
fn format_read_response(
    path: &str,
    content: &str,
    start_line: usize,
    limit: usize,
    plain: bool,
) -> String {
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();
    let start_index = start_line.saturating_sub(1).min(total_lines);
    let end_index = (start_index + limit).min(total_lines);
    // SAFETY: start and end are clamped to the collected line count above.
    #[allow(clippy::indexing_slicing)]
    let selected_lines = &lines[start_index..end_index];
    let formatted = if plain {
        selected_lines
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}|{}", start_line + index, line))
            .collect::<Vec<_>>()
            .join("\n")
    } else if selected_lines.is_empty() {
        String::new()
    } else {
        format_hash_lines(&selected_lines.join("\n"), start_line)
    };

    let mut header = format!("File: {path} ({total_lines} lines)");
    if start_line > 1 || end_index < total_lines {
        header.push_str(&format!(" [showing lines {}-{}]", start_line, end_index));
    }
    if end_index < total_lines {
        header.push_str(&format!(" ({} more lines below)", total_lines - end_index));
    }

    format!("{header}\n\n```\n{formatted}\n```")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsBackendError, FsReadResponse, FsWriteRequest, FsWriteResponse, ToolContext};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct CannedReadBackend {
        content: String,
        request: Mutex<Option<FsReadRequest>>,
    }

    #[async_trait]
    impl FsBackend for CannedReadBackend {
        /// Return canned content while recording the exact read request.
        async fn read_text_file(
            &self,
            request: FsReadRequest,
        ) -> Result<FsReadResponse, FsBackendError> {
            *self
                .request
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(FsReadResponse {
                content: self.content.clone(),
            })
        }

        /// This fake backend is only used by hashline read tests.
        async fn write_text_file(
            &self,
            _request: FsWriteRequest,
        ) -> Result<FsWriteResponse, FsBackendError> {
            Err(FsBackendError::Io("unexpected write".to_string()))
        }
    }

    /// Verifies that read_file returns hashline prefixes and a file header.
    #[tokio::test]
    async fn hashline_read_file_formats_hashlines() {
        let backend = Arc::new(CannedReadBackend {
            content: "alpha\nbeta".to_string(),
            request: Mutex::new(None),
        });
        let tool = HashlineReadFile::with_backend(backend);

        let result = tool
            .execute(
                serde_json::json!({"path": "sample.txt"}),
                &ToolContext::for_test("/workspace"),
            )
            .await
            .expect("read should succeed");

        assert!(result.contains("File: sample.txt (2 lines)"));
        assert!(result.contains("1:c8|alpha"));
        assert!(result.contains("2:89|beta"));
    }

    /// Verifies one-indexed pagination and plain output mode.
    #[tokio::test]
    async fn hashline_read_file_supports_offset_limit_and_plain() {
        let backend = Arc::new(CannedReadBackend {
            content: "one\ntwo\nthree\nfour".to_string(),
            request: Mutex::new(None),
        });
        let tool = HashlineReadFile::with_backend(backend);

        let result = tool
            .execute(
                serde_json::json!({"path": "sample.txt", "offset": 2, "limit": 2, "plain": true}),
                &ToolContext::for_test("/workspace"),
            )
            .await
            .expect("read should succeed");

        assert!(result.contains("[showing lines 2-3]"));
        assert!(result.contains("2|two\n3|three"));
        assert!(!result.contains("2:"));
    }

    /// Verifies that hashline reads request exact full-file content from the backend.
    #[tokio::test]
    async fn hashline_read_file_requests_full_content() {
        let backend = Arc::new(CannedReadBackend {
            content: "registered backend".to_string(),
            request: Mutex::new(None),
        });
        let tool = HashlineReadFile::with_backend(Arc::clone(&backend) as Arc<dyn FsBackend>);

        let _ = tool
            .execute(
                serde_json::json!({"path": "sample.txt"}),
                &ToolContext::for_test("/workspace"),
            )
            .await
            .expect("read should succeed");

        let request = backend
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("read request should be recorded");
        assert!(request.preserve_full);
        assert_eq!(request.offset, 0);
        assert_eq!(request.limit, None);
    }
}
