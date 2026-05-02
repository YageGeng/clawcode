use llm::completion::message::ToolChoice;
use tokio_util::sync::CancellationToken;

use crate::{
    runtime::{ToolBatchSummary, inflight::ToolCallRuntimeSnapshot, turn::LoopResult},
    session::SessionContinuationRequest,
    tools::{ToolApprovalHandler, ToolApprovalProfile, executor::ToolExecutionMode},
};

/// Computes an optional task-level continuation request from one completed loop result.
pub type ContinuationResolver =
    std::sync::Arc<dyn Fn(&LoopResult) -> Option<SessionContinuationRequest> + Send + Sync>;

/// Describes the runtime phase that a continuation hook is observing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationHookPhase {
    ToolBatchCompleted,
    BeforeFinalResponse,
    TurnCompleted,
}

/// Provides the hook with the phase and loop result that triggered continuation evaluation.
#[derive(Debug, Clone)]
pub struct ContinuationHookContext {
    pub phase: ContinuationHookPhase,
    pub loop_result: LoopResult,
    pub iteration: usize,
    pub tool_batch_summary: Option<ToolBatchSummary>,
    pub requested_continuation: Option<SessionContinuationRequest>,
    pub inflight_snapshot: ToolCallRuntimeSnapshot,
}

/// Describes how a continuation hook wants to affect the current turn's continuation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationHookDecision {
    Continue,
    Request(SessionContinuationRequest),
    Replace(SessionContinuationRequest),
}

/// Computes an optional task-level continuation request from a structured runtime hook context.
pub type ContinuationHook = std::sync::Arc<
    dyn Fn(&ContinuationHookContext) -> Option<SessionContinuationRequest> + Send + Sync,
>;

/// Computes a structured continuation decision from a runtime hook context.
pub type ContinuationDecisionHook =
    std::sync::Arc<dyn Fn(&ContinuationHookContext) -> ContinuationHookDecision + Send + Sync>;

/// Shared loop configuration consumed by both the task runner and the turn loop.
#[derive(Clone)]
pub struct AgentLoopConfig {
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    /// Configured policy: the deepest child-agent generation this runtime permits.
    /// `None` means unlimited. This is the single source of truth; every turn
    /// snapshots it into `AgentRuntimeContext.max_subagent_depth` for tool visibility.
    pub max_subagent_depth: Option<usize>,
    pub recent_message_limit: usize,
    pub skills: skills::SkillConfig,
    pub tool_choice: ToolChoice,
    pub tool_execution_mode: ToolExecutionMode,
    pub cancellation_token: Option<CancellationToken>,
    pub tool_approval_profile: ToolApprovalProfile,
    pub tool_approval_handler: Option<ToolApprovalHandler>,
    pub continuation_resolver: Option<ContinuationResolver>,
    pub continuation_hook: Option<ContinuationHook>,
    pub continuation_decision_hook: Option<ContinuationDecisionHook>,
}

impl std::fmt::Debug for AgentLoopConfig {
    /// Renders the runtime config without trying to print opaque callback internals.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_tool_calls", &self.max_tool_calls)
            .field("max_subagent_depth", &self.max_subagent_depth)
            .field("recent_message_limit", &self.recent_message_limit)
            .field("skills", &self.skills)
            .field("tool_choice", &self.tool_choice)
            .field("tool_execution_mode", &self.tool_execution_mode)
            .field(
                "cancellation_token",
                &self.cancellation_token.as_ref().map(|_| "<token>"),
            )
            .field("tool_approval_profile", &self.tool_approval_profile)
            .field(
                "tool_approval_handler",
                &self.tool_approval_handler.as_ref().map(|_| "<function>"),
            )
            .field(
                "continuation_resolver",
                &self.continuation_resolver.as_ref().map(|_| "<function>"),
            )
            .field(
                "continuation_hook",
                &self.continuation_hook.as_ref().map(|_| "<function>"),
            )
            .field(
                "continuation_decision_hook",
                &self
                    .continuation_decision_hook
                    .as_ref()
                    .map(|_| "<function>"),
            )
            .finish()
    }
}

impl Default for AgentLoopConfig {
    /// Builds the default loop config used across the runtime entry points.
    fn default() -> Self {
        Self {
            max_iterations: 8,
            // Keep tool-call cap disabled by default to avoid hard-baked execution ceilings.
            max_tool_calls: usize::MAX,
            max_subagent_depth: None,
            recent_message_limit: 24,
            skills: skills::SkillConfig::default(),
            tool_choice: ToolChoice::Auto,
            tool_execution_mode: ToolExecutionMode::Serial,
            cancellation_token: None,
            tool_approval_profile: ToolApprovalProfile::TrustAll,
            tool_approval_handler: None,
            continuation_resolver: None,
            continuation_hook: None,
            continuation_decision_hook: None,
        }
    }
}

impl AgentLoopConfig {
    /// Selects the runtime tool approval profile.
    pub fn with_tool_approval_profile(
        mut self,
        tool_approval_profile: ToolApprovalProfile,
    ) -> Self {
        self.tool_approval_profile = tool_approval_profile;
        self
    }

    /// Caps the deepest child-agent generation that may be spawned from this runtime.
    pub fn with_max_subagent_depth(mut self, max_subagent_depth: Option<usize>) -> Self {
        self.max_subagent_depth = max_subagent_depth;
        self
    }

    /// Installs an approval hook for tools requiring explicit user confirmation.
    pub fn with_tool_approval_handler(
        mut self,
        handler: impl Fn(crate::tools::ToolApprovalRequest) -> crate::tools::ToolApprovalFuture
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.tool_approval_handler = Some(std::sync::Arc::new(handler));
        self
    }

    /// Selects how completed tool calls should be drained after each stream iteration.
    pub fn with_tool_execution_mode(mut self, tool_execution_mode: ToolExecutionMode) -> Self {
        self.tool_execution_mode = tool_execution_mode;
        self
    }

    /// Installs a cancellation token used to abort in-flight tool execution batches.
    pub fn with_cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    /// Installs a hook that can request another outer-task turn from a completed loop result.
    pub fn with_continuation_resolver(
        mut self,
        resolver: impl Fn(&LoopResult) -> Option<SessionContinuationRequest> + Send + Sync + 'static,
    ) -> Self {
        self.continuation_resolver = Some(std::sync::Arc::new(resolver));
        self
    }

    /// Installs a structured continuation hook that can request another task turn from a runtime phase.
    pub fn with_continuation_hook(
        mut self,
        hook: impl Fn(&ContinuationHookContext) -> Option<SessionContinuationRequest>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.continuation_hook = Some(std::sync::Arc::new(hook));
        self
    }

    /// Installs a structured continuation-decision hook that can preserve or replace existing requests.
    pub fn with_continuation_decision_hook(
        mut self,
        hook: impl Fn(&ContinuationHookContext) -> ContinuationHookDecision + Send + Sync + 'static,
    ) -> Self {
        self.continuation_decision_hook = Some(std::sync::Arc::new(hook));
        self
    }
}
