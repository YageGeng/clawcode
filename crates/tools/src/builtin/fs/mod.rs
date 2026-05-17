//! Built-in file-system tools and registration.

pub mod edit;
pub mod patch;
pub mod read;
pub mod write;

use std::sync::Arc;

use crate::{FsBackend, LocalFsBackend, ToolRegistry};

impl ToolRegistry {
    /// Register built-in file-system tools.
    pub fn register_fs_tools(&self, is_anthropic: bool) {
        self.register_fs_tools_with_backend(is_anthropic, Arc::new(LocalFsBackend::new()));
    }

    /// Register built-in file-system tools using the provided backend.
    pub fn register_fs_tools_with_backend(&self, is_anthropic: bool, backend: Arc<dyn FsBackend>) {
        self.register(Arc::new(read::ReadFile::with_backend(Arc::clone(&backend))));
        self.register(Arc::new(write::WriteFile::with_backend(Arc::clone(
            &backend,
        ))));

        if is_anthropic {
            self.register(Arc::new(edit::EditFile::new()));
        } else {
            self.register(Arc::new(patch::ApplyPatch::new()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsBackend, FsBackendError, FsReadRequest, FsReadResponse, ToolContext};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct CannedBackend;

    #[async_trait]
    impl FsBackend for CannedBackend {
        /// Return canned content for registry injection tests.
        async fn read_text_file(
            &self,
            _request: FsReadRequest,
        ) -> Result<FsReadResponse, FsBackendError> {
            Ok(FsReadResponse {
                content: "registered backend".to_string(),
            })
        }

        /// This fake backend is only used by read registration tests.
        async fn write_text_file(
            &self,
            _request: crate::FsWriteRequest,
        ) -> Result<crate::FsWriteResponse, FsBackendError> {
            Err(FsBackendError::Io("unexpected write".to_string()))
        }
    }

    #[tokio::test]
    async fn register_fs_tools_with_backend_uses_injected_backend() {
        let registry = ToolRegistry::new();
        registry.register_fs_tools_with_backend(false, Arc::new(CannedBackend));

        let result = registry
            .execute(
                "read_file",
                serde_json::json!({"path": "sample.txt"}),
                &ToolContext::for_test("/workspace"),
            )
            .await
            .expect("registered read tool should use injected backend");

        assert_eq!(result, "registered backend");
    }
}
