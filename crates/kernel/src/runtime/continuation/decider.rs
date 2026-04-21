use crate::{
    Result,
    events::{
        TaskContinuationDecisionKind, TaskContinuationDecisionStage,
        TaskContinuationDecisionTraceEntry, TaskContinuationSource,
    },
    session::{SessionContinuationRequest, SessionStore},
};

use super::{
    AgentLoopConfig, ContinuationHookContext, ContinuationHookDecision, ContinuationHookPhase,
};
use crate::runtime::{task::RunRequest, turn::LoopResult};

/// Internal task-level decision describing whether the outer runtime loop should continue.
#[derive(Debug)]
pub(crate) enum TaskContinuation {
    Finish,
    Continue(SessionContinuationRequest),
}

impl TaskContinuation {
    /// Converts the queued continuation request into the next turn request.
    pub(crate) fn into_run_request(
        self,
        session_id: crate::session::SessionId,
        thread_id: crate::session::ThreadId,
    ) -> Option<RunRequest> {
        match self {
            Self::Finish => None,
            Self::Continue(SessionContinuationRequest::PendingInput { input }) => {
                Some(RunRequest::new(session_id, thread_id, input))
            }
            Self::Continue(SessionContinuationRequest::SystemFollowUp { input }) => {
                Some(RunRequest::new(session_id, thread_id, input))
            }
        }
    }

    /// Returns the public continuation action emitted by the outer task loop.
    pub(crate) fn action(&self) -> crate::events::TaskContinuationAction {
        match self {
            Self::Finish => crate::events::TaskContinuationAction::Finish,
            Self::Continue(_) => crate::events::TaskContinuationAction::Continue,
        }
    }

    /// Returns the source that caused the outer task loop to continue or finish.
    pub(crate) fn source(&self) -> TaskContinuationSource {
        match self {
            Self::Finish => TaskContinuationSource::TaskCompleted,
            Self::Continue(SessionContinuationRequest::PendingInput { .. }) => {
                TaskContinuationSource::PendingInput
            }
            Self::Continue(SessionContinuationRequest::SystemFollowUp { .. }) => {
                TaskContinuationSource::SystemFollowUp
            }
        }
    }
}

/// Converts one task-continuation outcome into the final public trace entry recorded for the turn.
impl From<&TaskContinuation> for TaskContinuationDecisionTraceEntry {
    fn from(continuation: &TaskContinuation) -> Self {
        match continuation {
            TaskContinuation::Finish => Self {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Finished,
                source: Some(TaskContinuationSource::TaskCompleted),
            },
            TaskContinuation::Continue(continuation) => Self {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Adopted,
                source: Some(TaskContinuationSource::from(continuation)),
            },
        }
    }
}

/// Decides whether the outer task loop should submit another turn after the current one.
pub(crate) async fn decide_task_continuation<S>(
    store: &S,
    request: &RunRequest,
    loop_result: &LoopResult,
    config: &AgentLoopConfig,
) -> Result<(TaskContinuation, Vec<TaskContinuationDecisionTraceEntry>)>
where
    S: SessionStore + ?Sized,
{
    let mut continuation = loop_result.requested_continuation.clone();
    let mut trace = loop_result.continuation_decision_trace.clone();

    if let Some(hook) = config.continuation_decision_hook.as_ref() {
        let hook_context = ContinuationHookContext {
            phase: ContinuationHookPhase::TurnCompleted,
            loop_result: loop_result.clone(),
            iteration: loop_result.iterations,
            tool_batch_summary: None,
            requested_continuation: continuation.clone(),
            inflight_snapshot: loop_result.inflight_snapshot.clone(),
        };
        match hook(&hook_context) {
            ContinuationHookDecision::Continue => {}
            ContinuationHookDecision::Request(requested_continuation) => {
                if continuation.is_none() {
                    continuation = Some(requested_continuation.clone());
                }
                trace.push(TaskContinuationDecisionTraceEntry {
                    stage: TaskContinuationDecisionStage::TurnCompletedHook,
                    decision: TaskContinuationDecisionKind::Request,
                    source: Some(TaskContinuationSource::from(&requested_continuation)),
                });
            }
            ContinuationHookDecision::Replace(replacement_continuation) => {
                continuation = Some(replacement_continuation.clone());
                trace.push(TaskContinuationDecisionTraceEntry {
                    stage: TaskContinuationDecisionStage::TurnCompletedHook,
                    decision: TaskContinuationDecisionKind::Replace,
                    source: Some(TaskContinuationSource::from(&replacement_continuation)),
                });
            }
        }
    } else if let Some(hook) = config.continuation_hook.as_ref() {
        let hook_context = ContinuationHookContext {
            phase: ContinuationHookPhase::TurnCompleted,
            loop_result: loop_result.clone(),
            iteration: loop_result.iterations,
            tool_batch_summary: None,
            requested_continuation: continuation.clone(),
            inflight_snapshot: loop_result.inflight_snapshot.clone(),
        };
        if let Some(requested_continuation) = hook(&hook_context) {
            if continuation.is_none() {
                continuation = Some(requested_continuation.clone());
            }
            trace.push(TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::TurnCompletedHook,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::from(&requested_continuation)),
            });
        }
    }

    if let Some(continuation) = continuation {
        return Ok((TaskContinuation::Continue(continuation), trace));
    }

    if let Some(resolver) = config.continuation_resolver.as_ref()
        && let Some(continuation) = resolver(loop_result)
    {
        trace.push(TaskContinuationDecisionTraceEntry {
            stage: TaskContinuationDecisionStage::Resolver,
            decision: TaskContinuationDecisionKind::Request,
            source: Some(TaskContinuationSource::from(&continuation)),
        });
        return Ok((TaskContinuation::Continue(continuation), trace));
    }

    let continuation = store
        .take_continuation(request.session_id.clone(), request.thread_id.clone())
        .await?;
    Ok(match continuation {
        Some(continuation) => {
            trace.push(TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::SessionQueue,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::from(&continuation)),
            });
            (TaskContinuation::Continue(continuation), trace)
        }
        None => (TaskContinuation::Finish, trace),
    })
}
