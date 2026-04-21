mod config;
mod decider;
mod hook;
pub use config::{
    AgentLoopConfig, ContinuationDecisionHook, ContinuationHook, ContinuationHookContext,
    ContinuationHookDecision, ContinuationHookPhase, ContinuationResolver,
};
pub(crate) use decider::decide_task_continuation;
pub(crate) use hook::{apply_hook_decision, run_continuation_hook, trace_entry_for_hook_decision};
