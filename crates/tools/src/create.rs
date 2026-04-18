use std::path::PathBuf;

use crate::{ToolRouter, build_default_tool_registry_plan};

/// Builds the default tool router rooted at the current workspace.
pub async fn create_default_tool_router() -> ToolRouter {
    create_file_tool_router_with_root(PathBuf::from(".")).await
}

/// Builds a file-tool router rooted at the provided directory.
pub async fn create_file_tool_router_with_root(root_dir: impl Into<PathBuf>) -> ToolRouter {
    let root_dir = root_dir.into();
    let plan = build_default_tool_registry_plan(&root_dir);
    plan.build_builder(&root_dir).build_router()
}
