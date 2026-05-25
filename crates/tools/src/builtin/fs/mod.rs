//! Built-in file-system tools and registration.

pub mod hashline;
pub mod legacy;

use std::sync::Arc;

use crate::{FsBackend, LocalFsBackend, ToolRegistry};

/// Selects which built-in file-system tool set should be model-visible.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FsToolSet {
    /// Preserve the historical read/write plus edit-or-apply-patch tools.
    #[default]
    Legacy,
    /// Use hashline-backed read/write/edit tools.
    Hashline,
}

impl ToolRegistry {
    /// Register built-in file-system tools.
    pub fn register_fs_tools(&self, is_anthropic: bool) {
        self.register_fs_tools_with_backend(is_anthropic, Arc::new(LocalFsBackend::new()));
    }

    /// Register built-in file-system tools from the selected tool set.
    pub fn register_fs_tools_with_set(&self, is_anthropic: bool, tool_set: FsToolSet) {
        self.register_fs_tools_with_backend_and_set(
            is_anthropic,
            Arc::new(LocalFsBackend::new()),
            tool_set,
        );
    }

    /// Register built-in file-system tools using the provided backend.
    pub fn register_fs_tools_with_backend(&self, is_anthropic: bool, backend: Arc<dyn FsBackend>) {
        self.register_fs_tools_with_backend_and_set(is_anthropic, backend, FsToolSet::Legacy);
    }

    /// Register built-in file-system tools using the selected tool set and backend.
    pub fn register_fs_tools_with_backend_and_set(
        &self,
        is_anthropic: bool,
        backend: Arc<dyn FsBackend>,
        tool_set: FsToolSet,
    ) {
        match tool_set {
            FsToolSet::Legacy => self.register_legacy_fs_tools(is_anthropic, backend),
            FsToolSet::Hashline => self.register_hashline_fs_tools(backend),
        }
    }

    /// Register the historical file-system tool set.
    fn register_legacy_fs_tools(&self, is_anthropic: bool, backend: Arc<dyn FsBackend>) {
        self.register(Arc::new(legacy::read::ReadFile::with_backend(Arc::clone(
            &backend,
        ))));
        self.register(Arc::new(legacy::write::WriteFile::with_backend(
            Arc::clone(&backend),
        )));

        if is_anthropic {
            self.register(Arc::new(legacy::edit::EditFile::new()));
        } else {
            self.register(Arc::new(legacy::apply_patch::ApplyPatch::new()));
        }
    }

    /// Register the hashline file-system tool set.
    fn register_hashline_fs_tools(&self, backend: Arc<dyn FsBackend>) {
        self.register(Arc::new(hashline::read::HashlineReadFile::with_backend(
            Arc::clone(&backend),
        )));
        self.register(Arc::new(hashline::write::HashlineWriteFile::with_backend(
            Arc::clone(&backend),
        )));
        self.register(Arc::new(hashline::edit::HashlineEditFile::with_backend(
            Arc::clone(&backend),
        )));
        if hashline::grep::HashlineGrep::is_available() {
            self.register(Arc::new(hashline::grep::HashlineGrep::new()));
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

    /// Verifies that explicit legacy registration keeps the apply_patch tool visible.
    #[test]
    fn register_fs_tools_with_set_legacy_registers_apply_patch() {
        let registry = ToolRegistry::new();
        registry.register_fs_tools_with_set(false, FsToolSet::Legacy);

        assert!(registry.get("read_file").is_some());
        assert!(registry.get("write_file").is_some());
        assert!(registry.get("apply_patch").is_some());
        assert!(registry.get("edit").is_none());
    }

    /// Verifies that explicit legacy Anthropic registration keeps the edit tool visible.
    #[test]
    fn register_fs_tools_with_set_legacy_registers_anthropic_edit() {
        let registry = ToolRegistry::new();
        registry.register_fs_tools_with_set(true, FsToolSet::Legacy);

        assert!(registry.get("read_file").is_some());
        assert!(registry.get("write_file").is_some());
        assert!(registry.get("edit").is_some());
        assert!(registry.get("apply_patch").is_none());
    }

    /// Verifies that hashline registration exposes hashline tools under replacement names.
    #[test]
    fn register_fs_tools_with_set_hashline_replaces_legacy_edit_tools() {
        let registry = ToolRegistry::new();
        registry.register_fs_tools_with_set(false, FsToolSet::Hashline);

        assert!(registry.get("read_file").is_some());
        assert!(registry.get("write_file").is_some());
        assert!(registry.get("edit_file").is_some());
        assert_eq!(
            registry.get("grep").is_some(),
            hashline::grep::HashlineGrep::is_available()
        );
        assert!(registry.get("apply_patch").is_none());
        assert!(registry.get("edit").is_none());
    }
}
