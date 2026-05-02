use std::{fmt, path::Path, sync::Arc};

use crate::{
    builtin::{
        apply_patch::ApplyPatchTool,
        collaboration::{
            CloseAgentTool, ListAgentsTool, SendAgentInputTool, SpawnAgentTool, WaitAgentTool,
        },
        read_text_file::ReadTextFileTool,
        shell::{ExecCommandTool, UnifiedExecRuntime, WriteStdinTool},
        write_text_file::WriteTextFileTool,
    },
    handler::ToolHandler,
    registry::ToolRegistryBuilder,
    spec::{ConfiguredToolSpec, ToolSpec},
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

    /// Adds one handler-backed spec while preserving handler-derived visibility metadata.
    pub fn push_handler_spec(
        &mut self,
        handler: Arc<dyn ToolHandler>,
        supports_parallel_tool_calls: bool,
    ) {
        let spec = ToolSpec::function_with_prompt(handler.definition(), handler.prompt_metadata());
        self.specs.push(ConfiguredToolSpec::new(
            spec,
            supports_parallel_tool_calls,
            handler.visible_when(),
        ));
        self.register_handler(handler);
    }

    /// Adds one visible tool spec to the plan.
    pub fn push_spec(&mut self, spec: ToolSpec, supports_parallel_tool_calls: bool) {
        self.specs.push(ConfiguredToolSpec::new(
            spec,
            supports_parallel_tool_calls,
            None,
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
            builder.push_configured_spec(spec.clone());
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
    plan.push_handler_spec(read_text_file, false);

    let write_text_file = Arc::new(WriteTextFileTool::new(root_dir));
    plan.push_handler_spec(write_text_file, false);

    let apply_patch = Arc::new(ApplyPatchTool::new(root_dir));
    plan.push_handler_spec(apply_patch, false);

    let shell_runtime = Arc::new(UnifiedExecRuntime::new(root_dir));
    let exec_command = Arc::new(ExecCommandTool::new(Arc::clone(&shell_runtime)));
    plan.push_handler_spec(exec_command, true);

    let write_stdin = Arc::new(WriteStdinTool::new(shell_runtime));
    plan.push_handler_spec(write_stdin, false);

    let spawn_agent = Arc::new(SpawnAgentTool);
    plan.push_handler_spec(spawn_agent, false);

    let send_agent_input = Arc::new(SendAgentInputTool);
    plan.push_handler_spec(send_agent_input, false);

    let wait_agent = Arc::new(WaitAgentTool);
    plan.push_handler_spec(wait_agent, false);

    let close_agent = Arc::new(CloseAgentTool);
    plan.push_handler_spec(close_agent, false);

    let list_agents = Arc::new(ListAgentsTool);
    plan.push_handler_spec(list_agents, false);

    plan
}
