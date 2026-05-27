//! Filesystem backend abstraction used by built-in file tools.

use std::path::PathBuf;

use async_trait::async_trait;
use protocol::SessionId;
use thiserror::Error;
use tokio::fs;

/// Error returned by filesystem backend implementations.
#[derive(Debug, Error)]
pub enum FsBackendError {
    /// The requested path or range was invalid for the backend.
    #[error("{0}")]
    InvalidRequest(String),
    /// A filesystem or transport operation failed.
    #[error("{0}")]
    Io(String),
}

/// Request to read text through a filesystem backend.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct FsReadRequest {
    /// Session id used by session-scoped backends.
    pub session_id: SessionId,
    /// Working directory used to resolve relative paths.
    pub cwd: PathBuf,
    /// User-provided path from the read tool input.
    pub path: PathBuf,
    /// Zero-based starting line offset.
    pub offset: usize,
    /// Optional maximum number of lines to return.
    #[builder(default, setter(strip_option))]
    pub limit: Option<usize>,
    /// Return exact file content without line slicing when set.
    #[builder(default)]
    pub preserve_full: bool,
}

/// Response returned by a backend read operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsReadResponse {
    /// Text content returned to the read tool.
    pub content: String,
}

/// Request to write text through a filesystem backend.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct FsWriteRequest {
    /// Session id used by session-scoped backends.
    pub session_id: SessionId,
    /// Working directory used to resolve relative paths.
    pub cwd: PathBuf,
    /// User-provided path from the write tool input.
    pub path: PathBuf,
    /// Exact UTF-8 text content to write.
    pub content: String,
}

/// Response returned by a backend write operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsWriteResponse {
    /// Number of bytes written.
    pub bytes_written: usize,
    /// Display path used in tool output.
    pub display_path: PathBuf,
}

/// Backend used by built-in file tools to perform text file I/O.
#[async_trait]
pub trait FsBackend: Send + Sync {
    /// Read a UTF-8 text file for the read tool.
    async fn read_text_file(
        &self,
        request: FsReadRequest,
    ) -> Result<FsReadResponse, FsBackendError>;

    /// Write UTF-8 text content for the write tool.
    async fn write_text_file(
        &self,
        request: FsWriteRequest,
    ) -> Result<FsWriteResponse, FsBackendError>;
}

/// Local filesystem backend preserving the original built-in tool behaviour.
#[derive(Debug, Default)]
pub struct LocalFsBackend;

impl LocalFsBackend {
    /// Create a local filesystem backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Resolve a user path relative to cwd when needed.
    fn resolve_path(cwd: &std::path::Path, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    }
}

#[async_trait]
impl FsBackend for LocalFsBackend {
    /// Read a text file from the local filesystem.
    async fn read_text_file(
        &self,
        request: FsReadRequest,
    ) -> Result<FsReadResponse, FsBackendError> {
        let resolved = Self::resolve_path(&request.cwd, request.path);
        // Canonicalize to preserve the existing read tool's symlink-escape detection.
        let resolved = fs::canonicalize(&resolved).await.map_err(|e| {
            FsBackendError::Io(format!(
                "failed to resolve {}: {e}",
                resolved.display()
            ))
        })?;

        let content = fs::read_to_string(&resolved).await.map_err(|e| {
            FsBackendError::Io(format!(
                "failed to read {}: {e}",
                resolved.display()
            ))
        })?;

        if request.preserve_full
            && request.offset == 0
            && request.limit.is_none()
        {
            return Ok(FsReadResponse { content });
        }

        let lines: Vec<&str> = content.lines().collect();
        let start = request.offset.min(lines.len());
        let end = request
            .limit
            .map(|line_count| (start + line_count).min(lines.len()))
            .unwrap_or(lines.len());

        // SAFETY: both `start` and `end` are clamped to `lines.len()` above.
        #[allow(clippy::indexing_slicing)]
        Ok(FsReadResponse {
            content: lines[start..end].join("\n"),
        })
    }

    /// Write text content to the local filesystem.
    async fn write_text_file(
        &self,
        request: FsWriteRequest,
    ) -> Result<FsWriteResponse, FsBackendError> {
        let resolved = Self::resolve_path(&request.cwd, request.path);

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                FsBackendError::Io(format!("failed to create parent dir: {e}"))
            })?;
        }

        fs::write(&resolved, &request.content).await.map_err(|e| {
            FsBackendError::Io(format!(
                "failed to write {}: {e}",
                resolved.display()
            ))
        })?;

        Ok(FsWriteResponse {
            bytes_written: request.content.len(),
            display_path: resolved,
        })
    }
}
