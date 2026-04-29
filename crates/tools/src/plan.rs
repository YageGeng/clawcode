use std::{fmt, path::Path, sync::Arc};

use crate::{
    builtin::{
        apply_patch::ApplyPatchTool,
        read_text_file::ReadTextFileTool,
        shell::{ExecCommandTool, UnifiedExecRuntime, WriteStdinTool},
        write_text_file::WriteTextFileTool,
    },
    handler::ToolHandler,
    registry::ToolRegistryBuilder,
    spec::ToolSpec,
};

/// Stores one dispatch registration including the already-constructed handler.
pub struct PlannedToolHandler {
    pub handler: Arc<dyn ToolHandler>,
}

impl Clone for PlannedToolHandler {
    fn clone(&self) -> Self {
        Self {
            handler: Arc::clone(&self.handler),
        }
    }
}

impl fmt::Debug for PlannedToolHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlannedToolHandler")
            .field("handler", &self.handler.name())
            .finish()
    }
}

/// Captures the visible specs plus dispatch registrations needed to build a router.
#[derive(Debug, Clone)]
pub struct ToolRegistryPlan {
    pub specs: Vec<crate::ConfiguredToolSpec>,
    pub handlers: Vec<PlannedToolHandler>,
}

impl ToolRegistryPlan {
    /// Creates an empty registry plan.
    pub fn new() -> Self {
        Self {
            specs: Vec::new(),
            handlers: Vec::new(),
        }
    }

    /// Adds one visible tool spec to the plan.
    pub fn push_spec(&mut self, spec: ToolSpec, supports_parallel_tool_calls: bool) {
        self.specs.push(crate::ConfiguredToolSpec::new(
            spec,
            supports_parallel_tool_calls,
        ));
    }

    /// Registers a pre-constructed tool handler for dispatch.
    pub fn register_handler(&mut self, handler: Arc<dyn ToolHandler>) {
        self.handlers.push(PlannedToolHandler { handler });
    }

    /// Resolves this plan into a concrete registry builder rooted at the provided directory.
    pub fn build_builder(&self, _root_dir: &Path) -> ToolRegistryBuilder {
        let mut builder = ToolRegistryBuilder::new();
        for spec in &self.specs {
            builder.push_spec(spec.spec.clone(), spec.supports_parallel_tool_calls);
        }
        for planned in &self.handlers {
            builder.register_handler(planned.handler.name(), Arc::clone(&planned.handler));
        }
        builder
    }
}

impl Default for ToolRegistryPlan {
    /// Builds the default empty plan.
    fn default() -> Self {
        Self::new()
    }
}

/// Builds the default local plan for file, patch, and shell tools.
pub fn build_default_tool_registry_plan(root_dir: impl AsRef<Path>) -> ToolRegistryPlan {
    let root_dir = root_dir.as_ref();
    let mut plan = ToolRegistryPlan::new();

    let read_text_file = Arc::new(ReadTextFileTool::new(root_dir));
    plan.push_spec(
        ToolSpec::function_with_prompt(
            read_text_file.definition(),
            read_text_file.prompt_metadata(),
        ),
        false,
    );
    plan.register_handler(read_text_file);

    let write_text_file = Arc::new(WriteTextFileTool::new(root_dir));
    plan.push_spec(
        ToolSpec::function_with_prompt(
            write_text_file.definition(),
            write_text_file.prompt_metadata(),
        ),
        false,
    );
    plan.register_handler(write_text_file);

    let apply_patch = Arc::new(ApplyPatchTool::new(root_dir));
    plan.push_spec(
        ToolSpec::function_with_prompt(apply_patch.definition(), apply_patch.prompt_metadata()),
        false,
    );
    plan.register_handler(apply_patch);

    let shell_runtime = Arc::new(UnifiedExecRuntime::new(root_dir));
    let exec_command = Arc::new(ExecCommandTool::new(Arc::clone(&shell_runtime)));
    plan.push_spec(
        ToolSpec::function_with_prompt(exec_command.definition(), exec_command.prompt_metadata()),
        true,
    );
    plan.register_handler(exec_command);

    let write_stdin = Arc::new(WriteStdinTool::new(shell_runtime));
    plan.push_spec(
        ToolSpec::function_with_prompt(write_stdin.definition(), write_stdin.prompt_metadata()),
        false,
    );
    plan.register_handler(write_stdin);

    plan
}
