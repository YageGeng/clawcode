use std::{path::Path, sync::Arc};

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

/// Enumerates the builtin handler families supported by the local tools crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolHandlerKind {
    ApplyPatch,
    ExecCommand,
    ReadTextFile,
    WriteTextFile,
    WriteStdin,
}

/// Stores one dispatch registration emitted by the registry plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedToolHandler {
    pub name: String,
    pub kind: ToolHandlerKind,
}

/// Captures the visible specs plus dispatch registrations needed to build a router.
#[derive(Debug, Clone, PartialEq)]
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

    /// Adds one dispatch registration to the plan.
    pub fn register_handler(&mut self, name: impl Into<String>, kind: ToolHandlerKind) {
        self.handlers.push(PlannedToolHandler {
            name: name.into(),
            kind,
        });
    }

    /// Resolves this plan into a concrete registry builder rooted at the provided directory.
    pub fn build_builder(&self, root_dir: &Path) -> ToolRegistryBuilder {
        let mut builder = ToolRegistryBuilder::new();
        let shell_runtime = Arc::new(UnifiedExecRuntime::new(root_dir));
        for spec in &self.specs {
            builder.push_spec(spec.spec.clone(), spec.supports_parallel_tool_calls);
        }
        for handler in &self.handlers {
            builder.register_handler(
                handler.name.clone(),
                instantiate_handler(handler.kind, root_dir, &shell_runtime),
            );
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

    let read_text_file = ReadTextFileTool::new(root_dir);
    plan.push_spec(ToolSpec::function(read_text_file.definition()), false);
    plan.register_handler(read_text_file.name(), ToolHandlerKind::ReadTextFile);
    let write_text_file = WriteTextFileTool::new(root_dir);
    plan.push_spec(ToolSpec::function(write_text_file.definition()), false);
    plan.register_handler(write_text_file.name(), ToolHandlerKind::WriteTextFile);
    let apply_patch = ApplyPatchTool::new(root_dir);
    plan.push_spec(ToolSpec::function(apply_patch.definition()), false);
    plan.register_handler(apply_patch.name(), ToolHandlerKind::ApplyPatch);
    let shell_runtime = Arc::new(UnifiedExecRuntime::new(root_dir));
    let exec_command = ExecCommandTool::new(Arc::clone(&shell_runtime));
    plan.push_spec(ToolSpec::function(exec_command.definition()), true);
    plan.register_handler(exec_command.name(), ToolHandlerKind::ExecCommand);
    let write_stdin = WriteStdinTool::new(shell_runtime);
    plan.push_spec(ToolSpec::function(write_stdin.definition()), false);
    plan.register_handler(write_stdin.name(), ToolHandlerKind::WriteStdin);

    plan
}

/// Instantiates one builtin handler from the plan kind plus workspace root.
fn instantiate_handler(
    kind: ToolHandlerKind,
    root_dir: &Path,
    shell_runtime: &Arc<UnifiedExecRuntime>,
) -> Arc<dyn ToolHandler> {
    match kind {
        ToolHandlerKind::ApplyPatch => Arc::new(ApplyPatchTool::new(root_dir)),
        ToolHandlerKind::ExecCommand => Arc::new(ExecCommandTool::new(Arc::clone(shell_runtime))),
        ToolHandlerKind::ReadTextFile => Arc::new(ReadTextFileTool::new(root_dir)),
        ToolHandlerKind::WriteTextFile => Arc::new(WriteTextFileTool::new(root_dir)),
        ToolHandlerKind::WriteStdin => Arc::new(WriteStdinTool::new(Arc::clone(shell_runtime))),
    }
}
