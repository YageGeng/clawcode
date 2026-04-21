use crate::{
    events::{
        TaskContinuationDecisionKind, TaskContinuationDecisionStage,
        TaskContinuationDecisionTraceEntry, TaskContinuationSource,
    },
    session::SessionContinuationRequest,
};

use super::{
    AgentLoopConfig, ContinuationHookContext, ContinuationHookDecision, ContinuationHookPhase,
};
use crate::runtime::{ToolBatchSummary, turn::LoopResult};

/// Converts one continuation request into the public event-layer source enum.
impl From<&SessionContinuationRequest> for TaskContinuationSource {
    fn from(continuation: &SessionContinuationRequest) -> Self {
        match continuation {
            SessionContinuationRequest::PendingInput { .. } => Self::PendingInput,
            SessionContinuationRequest::SystemFollowUp { .. } => Self::SystemFollowUp,
        }
    }
}

/// Converts one hook phase into the matching public decision-trace stage.
impl From<ContinuationHookPhase> for TaskContinuationDecisionStage {
    fn from(phase: ContinuationHookPhase) -> Self {
        match phase {
            ContinuationHookPhase::ToolBatchCompleted => Self::ToolBatchCompletedHook,
            ContinuationHookPhase::BeforeFinalResponse => Self::BeforeFinalResponseHook,
            ContinuationHookPhase::TurnCompleted => Self::TurnCompletedHook,
        }
    }
}

/// Runs the configured continuation hook for one runtime phase and returns its decision.
pub(crate) fn run_continuation_hook(
    config: &AgentLoopConfig,
    phase: ContinuationHookPhase,
    iteration: usize,
    loop_result: LoopResult,
    tool_batch_summary: Option<ToolBatchSummary>,
) -> ContinuationHookDecision {
    let requested_continuation = loop_result.requested_continuation.clone();
    let inflight_snapshot = loop_result.inflight_snapshot.clone();
    let context = ContinuationHookContext {
        phase,
        loop_result,
        iteration,
        tool_batch_summary,
        requested_continuation,
        inflight_snapshot,
    };

    if let Some(hook) = config.continuation_decision_hook.as_ref() {
        return hook(&context);
    }

    config
        .continuation_hook
        .as_ref()
        .and_then(|hook| hook(&context))
        .map_or(
            ContinuationHookDecision::Continue,
            ContinuationHookDecision::Request,
        )
}

/// Applies one hook decision to the currently requested continuation while preserving priority semantics.
pub(crate) fn apply_hook_decision(
    current: Option<SessionContinuationRequest>,
    decision: ContinuationHookDecision,
) -> Option<SessionContinuationRequest> {
    match decision {
        ContinuationHookDecision::Continue => current,
        ContinuationHookDecision::Request(continuation) => current.or(Some(continuation)),
        ContinuationHookDecision::Replace(continuation) => Some(continuation),
    }
}

/// Builds one public trace entry from a hook phase decision so events can expose hook reasoning.
pub(crate) fn trace_entry_for_hook_decision(
    phase: ContinuationHookPhase,
    decision: &ContinuationHookDecision,
) -> TaskContinuationDecisionTraceEntry {
    let (decision, source) = match decision {
        ContinuationHookDecision::Continue => (TaskContinuationDecisionKind::Continue, None),
        ContinuationHookDecision::Request(continuation) => (
            TaskContinuationDecisionKind::Request,
            Some(TaskContinuationSource::from(continuation)),
        ),
        ContinuationHookDecision::Replace(continuation) => (
            TaskContinuationDecisionKind::Replace,
            Some(TaskContinuationSource::from(continuation)),
        ),
    };
    TaskContinuationDecisionTraceEntry {
        stage: TaskContinuationDecisionStage::from(phase),
        decision,
        source,
    }
}
