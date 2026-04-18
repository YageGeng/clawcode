use std::sync::Arc;

use crate::{
    Result,
    context::{ToolCallRequest, ToolContext, ToolInvocation, ToolOutput},
    error::{MissingToolSnafu, ToolApprovalRequiredSnafu, ToolTimeoutSnafu},
    registry::ToolRegistry,
    spec::{ConfiguredToolSpec, ToolSpec},
};
use snafu::{OptionExt, ResultExt};

/// Dispatches tool calls against a tool registry while exposing model-visible specs.
pub struct ToolRouter {
    registry: Arc<ToolRegistry>,
    specs: Vec<ConfiguredToolSpec>,
}

impl ToolRouter {
    /// Builds a router from a pre-registered tool registry.
    pub fn new(registry: Arc<ToolRegistry>, specs: Vec<ConfiguredToolSpec>) -> Self {
        Self { registry, specs }
    }

    /// Returns the shared registry backing this router.
    pub fn registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.registry)
    }

    /// Returns the model-visible tool definitions for this router.
    pub async fn definitions(&self) -> Vec<llm::completion::ToolDefinition> {
        let mut definitions = self
            .specs
            .iter()
            .map(|configured| configured.spec.definition.clone())
            .collect::<Vec<_>>();
        definitions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        definitions
    }

    /// Returns the configured visible specs owned by this router.
    pub fn specs(&self) -> &[ConfiguredToolSpec] {
        &self.specs
    }

    /// Returns one visible spec by stable tool name.
    pub fn find_spec(&self, name: &str) -> Option<&ToolSpec> {
        self.specs
            .iter()
            .find(|configured| configured.name() == name)
            .map(|configured| &configured.spec)
    }

    /// Dispatches a single tool call through the registered handler set.
    pub async fn dispatch(
        &self,
        call: ToolCallRequest,
        context: ToolContext,
    ) -> Result<ToolOutput> {
        let invocation = ToolInvocation::from_call_request(call, context);
        let tool = self
            .registry
            .get(&invocation.tool_name)
            .await
            .context(MissingToolSnafu {
                tool: invocation.tool_name.clone(),
                stage: "tool-router-lookup".to_string(),
            })?;

        if invocation.context.enforce_tool_approvals
            && tool.metadata().approval == crate::ApprovalRequirement::Always
        {
            let approval_request = crate::ToolApprovalRequest {
                tool: invocation.tool_name.clone(),
                call_id: Some(invocation.effective_call_id()),
                session_id: invocation.context.session_id.clone(),
                thread_id: invocation.context.thread_id.clone(),
                arguments: invocation
                    .function_arguments()
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            };

            let approved = invocation
                .context
                .tool_approval_handler
                .as_ref()
                .is_some_and(|handler| handler(&approval_request));

            if !approved {
                return ToolApprovalRequiredSnafu {
                    tool: invocation.tool_name,
                    stage: "tool-router-approval".to_string(),
                }
                .fail();
            }
        }

        let timeout = tool.metadata().timeout;
        let tool_name = invocation.tool_name.clone();

        tokio::time::timeout(timeout, tool.handle(invocation))
            .await
            .context(ToolTimeoutSnafu {
                tool: tool_name,
                stage: "tool-router-timeout".to_string(),
            })?
    }
}
