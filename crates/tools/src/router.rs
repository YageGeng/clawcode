use std::{path::PathBuf, sync::Arc};

use crate::{
    Result,
    collaboration::AgentRuntimeContext,
    context::{ToolCallRequest, ToolContext, ToolInvocation, ToolOutput},
    error::{MissingToolSnafu, ToolApprovalRequiredSnafu, ToolTimeoutSnafu},
    plan::build_default_tool_registry_plan,
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
    /// Builds the default local tool router rooted at the provided workspace path.
    pub async fn from_path(root_dir: impl Into<PathBuf>) -> Self {
        let root_dir = root_dir.into();
        let plan = build_default_tool_registry_plan(&root_dir);
        plan.build_builder(&root_dir).build_router()
    }

    /// Builds a router from a pre-registered tool registry.
    pub fn new(registry: Arc<ToolRegistry>, specs: Vec<ConfiguredToolSpec>) -> Self {
        Self { registry, specs }
    }

    /// Returns a reference to the registry backing this router.
    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Returns the model-visible tool definitions for the supplied agent context.
    pub fn definitions_for_agent(
        &self,
        agent: &AgentRuntimeContext,
    ) -> Vec<llm::completion::ToolDefinition> {
        let mut definitions = self
            .visible_specs(agent)
            .iter()
            .map(|configured| configured.spec.definition.clone())
            .collect::<Vec<_>>();
        definitions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        definitions
    }

    /// Returns the model-visible tool definitions for the default root-agent context.
    #[deprecated = "use `definitions_for_agent` to ensure depth-aware visibility filtering"]
    pub fn definitions(&self) -> Vec<llm::completion::ToolDefinition> {
        self.definitions_for_agent(&AgentRuntimeContext::default())
    }

    /// Returns every configured visible spec owned by this router for the supplied agent context.
    pub fn visible_specs(&self, agent: &AgentRuntimeContext) -> Vec<&ConfiguredToolSpec> {
        self.specs
            .iter()
            .filter(|configured| self.tool_is_visible(configured.name(), agent))
            .collect()
    }

    /// Returns the configured visible specs owned by this router for the default root-agent context.
    #[deprecated = "use `visible_specs` to ensure depth-aware visibility filtering"]
    pub fn specs(&self) -> Vec<&ConfiguredToolSpec> {
        self.visible_specs(&AgentRuntimeContext::default())
    }

    /// Returns one visible spec by stable tool name for the supplied agent context.
    pub fn find_spec_for_agent(
        &self,
        name: &str,
        agent: &AgentRuntimeContext,
    ) -> Option<&ToolSpec> {
        self.visible_specs(agent)
            .iter()
            .find(|configured| configured.name() == name)
            .map(|configured| &configured.spec)
    }

    /// Returns one visible spec by stable tool name for the default root-agent context.
    #[deprecated = "use `find_spec_for_agent` to ensure depth-aware visibility filtering"]
    pub fn find_spec(&self, name: &str) -> Option<&ToolSpec> {
        self.find_spec_for_agent(name, &AgentRuntimeContext::default())
    }

    /// Returns whether the named tool may participate in a parallel execution batch.
    pub fn tool_supports_parallel_for_agent(
        &self,
        name: &str,
        agent: &AgentRuntimeContext,
    ) -> bool {
        self.visible_specs(agent)
            .iter()
            .find(|configured| configured.name() == name)
            .is_some_and(|configured| configured.supports_parallel_tool_calls)
    }

    /// Returns whether the named tool may participate in a parallel execution batch for the default root-agent context.
    #[deprecated = "use `tool_supports_parallel_for_agent` to ensure depth-aware visibility filtering"]
    pub fn tool_supports_parallel(&self, name: &str) -> bool {
        self.tool_supports_parallel_for_agent(name, &AgentRuntimeContext::default())
    }

    /// Dispatches a single tool call through the registered handler set.
    pub async fn dispatch(
        &self,
        call: ToolCallRequest,
        context: ToolContext,
    ) -> Result<ToolOutput> {
        let invocation = ToolInvocation::from_call_request(call, context);
        if !self.tool_is_visible(&invocation.tool_name, &invocation.context.agent) {
            return MissingToolSnafu {
                tool: invocation.tool_name.clone(),
                stage: "tool-router-visibility".to_string(),
            }
            .fail();
        }
        let tool = self
            .registry
            .get(&invocation.tool_name)
            .await
            .context(MissingToolSnafu {
                tool: invocation.tool_name.clone(),
                stage: "tool-router-lookup".to_string(),
            })?;

        if invocation
            .context
            .approval_profile
            .requires_approval(tool.metadata().approval)
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

            let approved = if let Some(handler) = invocation.context.tool_approval_handler.as_ref()
            {
                handler.approve(approval_request).await
            } else {
                false
            };

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

    /// Returns whether the named tool should be visible to the current agent.
    /// Delegates to the per-tool visibility predicate registered with each spec.
    fn tool_is_visible(&self, name: &str, agent: &AgentRuntimeContext) -> bool {
        self.specs
            .iter()
            .find(|configured| configured.name() == name)
            .is_none_or(|configured| configured.is_visible_to(agent))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        AgentRuntimeContext, ToolCallRequest, ToolContext, ToolRegistryBuilder,
        builtin::collaboration::{SpawnAgentTool, WaitAgentTool},
    };

    use super::ToolRouter;

    /// Builds a router that exposes the collaboration tool pair used by depth-limit tests.
    fn collaboration_router() -> ToolRouter {
        let mut builder = ToolRegistryBuilder::new();
        builder.push_handler_spec(Arc::new(SpawnAgentTool));
        builder.push_handler_spec(Arc::new(WaitAgentTool));
        builder.build_router()
    }

    /// Agents at their maximum configured depth no longer see `spawn_agent`.
    #[test]
    fn definitions_for_agent_hide_spawn_agent_at_depth_limit() {
        let router = collaboration_router();
        let tool_names = router
            .definitions_for_agent(&AgentRuntimeContext {
                subagent_depth: 1,
                max_subagent_depth: Some(1),
                ..AgentRuntimeContext::default()
            })
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();

        assert!(!tool_names.iter().any(|name| name == "spawn_agent"));
        assert!(tool_names.iter().any(|name| name == "wait_agent"));
    }

    /// Hidden collaboration tools behave as if they do not exist even if the handler remains registered.
    #[tokio::test]
    async fn dispatch_rejects_hidden_spawn_agent_calls() {
        let router = collaboration_router();
        let error = router
            .dispatch(
                ToolCallRequest::new("call-1", "spawn_agent", serde_json::json!({})),
                ToolContext::new("session", "thread").with_agent_runtime_context(
                    AgentRuntimeContext {
                        subagent_depth: 1,
                        max_subagent_depth: Some(1),
                        ..AgentRuntimeContext::default()
                    },
                ),
            )
            .await
            .expect_err("hidden spawn_agent should be rejected");

        assert!(matches!(
            error,
            crate::Error::MissingTool { stage, .. } if stage == "tool-router-visibility"
        ));
    }
}
