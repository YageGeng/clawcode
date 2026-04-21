use llm::{completion::Message, completion::message::ToolChoice, usage::Usage};
use std::fmt;
use tokio_util::sync::CancellationToken;

use super::inflight::ToolCallRuntimeSnapshot;
use super::sampling::{IterationOutcome, collect_stream_response};
use super::tool_runtime::{ToolExecutionRuntimeInput, execute_tool_execution_plan};

use crate::{
    Result,
    events::AgentStage,
    events::{
        AgentEvent, EventSink, TaskContinuationDecisionKind, TaskContinuationDecisionStage,
        TaskContinuationDecisionTraceEntry, TaskContinuationSource,
    },
    model::{AgentModel, ModelRequest},
    session::{SessionContinuationRequest, SessionId, SessionStore, ThreadId},
    tools::{ToolApprovalHandler, executor::ToolExecutionMode, router::ToolRouter},
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

/// One display-friendly summary entry describing a tool call that completed in the last batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBatchSummaryEntry {
    pub handle_id: String,
    pub name: String,
    pub tool_id: String,
    pub tool_call_id: String,
    pub output_summary: String,
}

/// Summary of the most recent tool batch completion that a continuation hook can inspect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolBatchSummary {
    pub entries: Vec<ToolBatchSummaryEntry>,
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

#[derive(Clone)]
pub struct AgentLoopConfig {
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub recent_message_limit: usize,
    pub tool_choice: ToolChoice,
    pub tool_execution_mode: ToolExecutionMode,
    pub cancellation_token: Option<CancellationToken>,
    pub enforce_tool_approvals: bool,
    pub tool_approval_handler: Option<ToolApprovalHandler>,
    pub continuation_resolver: Option<ContinuationResolver>,
    pub continuation_hook: Option<ContinuationHook>,
    pub continuation_decision_hook: Option<ContinuationDecisionHook>,
}

impl fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_tool_calls", &self.max_tool_calls)
            .field("recent_message_limit", &self.recent_message_limit)
            .field("tool_choice", &self.tool_choice)
            .field("tool_execution_mode", &self.tool_execution_mode)
            .field(
                "cancellation_token",
                &self.cancellation_token.as_ref().map(|_| "<token>"),
            )
            .field("enforce_tool_approvals", &self.enforce_tool_approvals)
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
    fn default() -> Self {
        Self {
            max_iterations: 8,
            max_tool_calls: 16,
            recent_message_limit: 24,
            tool_choice: ToolChoice::Auto,
            tool_execution_mode: ToolExecutionMode::Serial,
            cancellation_token: None,
            enforce_tool_approvals: false,
            tool_approval_handler: None,
            continuation_resolver: None,
            continuation_hook: None,
            continuation_decision_hook: None,
        }
    }
}

impl AgentLoopConfig {
    /// Sets whether tools marked as `ApprovalRequirement::Always` must pass approval.
    pub fn with_tool_approvals(mut self, enforce_tool_approvals: bool) -> Self {
        self.enforce_tool_approvals = enforce_tool_approvals;
        self
    }

    /// Installs an approval hook for tools requiring explicit user confirmation.
    pub fn with_tool_approval_handler(
        mut self,
        handler: impl Fn(&crate::tools::ToolApprovalRequest) -> bool + Send + Sync + 'static,
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

#[derive(Debug, Clone)]
pub struct LoopResult {
    pub final_text: String,
    pub usage: Usage,
    pub new_messages: Vec<Message>,
    pub iterations: usize,
    pub inflight_snapshot: ToolCallRuntimeSnapshot,
    pub requested_continuation: Option<SessionContinuationRequest>,
    pub continuation_decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
    pub(crate) next_tool_handle_sequence: usize,
}

#[derive(Debug, Clone)]
pub struct AgentLoopRequest {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub system_prompt: Option<String>,
    pub working_messages: Vec<Message>,
    pub next_tool_handle_sequence: usize,
}

/// Runs one turn through repeated sampling iterations until the model can respond.
pub(crate) async fn run_turn<M, E>(
    model: &M,
    store: &impl SessionStore,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    request: AgentLoopRequest,
) -> Result<LoopResult>
where
    M: AgentModel,
    E: EventSink,
{
    let AgentLoopRequest {
        session_id,
        thread_id,
        system_prompt,
        mut working_messages,
        mut next_tool_handle_sequence,
    } = request;
    let mut new_messages = Vec::new();
    let mut usage = Usage::new();
    let mut total_tool_calls = 0usize;
    let mut previous_response_id: Option<String> = None;
    let mut final_inflight_snapshot = ToolCallRuntimeSnapshot::default();
    let mut requested_continuation = None;
    let mut continuation_decision_trace = Vec::new();

    for iteration in 1..=config.max_iterations {
        let tool_definitions = router.definitions().await;
        events
            .publish(AgentEvent::StatusUpdated {
                stage: AgentStage::ModelRequesting,
                message: None,
                iteration: Some(iteration),
                tool_id: None,
                tool_call_id: None,
            })
            .await;
        events
            .publish(AgentEvent::ModelRequested {
                message_count: working_messages.len(),
                tool_count: tool_definitions.len(),
            })
            .await;

        let mut stream = model
            .stream(ModelRequest {
                system_prompt: system_prompt.clone(),
                messages: working_messages.clone(),
                tools: tool_definitions,
                tool_choice: config.tool_choice.clone(),
                previous_response_id: previous_response_id.clone(),
            })
            .await?;
        let iteration_result =
            collect_stream_response(events, iteration, next_tool_handle_sequence, &mut stream)
                .await?;

        usage += iteration_result.usage;
        previous_response_id = iteration_result.message_id.clone();
        next_tool_handle_sequence = iteration_result.in_flight_tool_calls.next_handle_sequence();

        match IterationOutcome::from(iteration_result) {
            IterationOutcome::Respond { message_id, text } => {
                let hook_decision = run_continuation_hook(
                    config,
                    ContinuationHookPhase::BeforeFinalResponse,
                    iteration,
                    LoopResult {
                        final_text: text.clone(),
                        usage,
                        new_messages: new_messages.clone(),
                        iterations: iteration,
                        inflight_snapshot: final_inflight_snapshot.clone(),
                        requested_continuation: requested_continuation.clone(),
                        continuation_decision_trace: continuation_decision_trace.clone(),
                        next_tool_handle_sequence,
                    },
                    None,
                );
                continuation_decision_trace.push(trace_entry_for_hook_decision(
                    ContinuationHookPhase::BeforeFinalResponse,
                    &hook_decision,
                ));
                requested_continuation =
                    apply_hook_decision(requested_continuation.clone(), hook_decision);
                events
                    .publish(AgentEvent::StatusUpdated {
                        stage: AgentStage::Responding,
                        message: Some(text.clone()),
                        iteration: Some(iteration),
                        tool_id: None,
                        tool_call_id: None,
                    })
                    .await;
                return finish_text_response(
                    store,
                    session_id.clone(),
                    thread_id.clone(),
                    events,
                    message_id,
                    text,
                    usage,
                    new_messages,
                    iteration,
                    final_inflight_snapshot,
                    requested_continuation,
                    continuation_decision_trace,
                    next_tool_handle_sequence,
                )
                .await;
            }
            IterationOutcome::ContinueWithTools(plan) => {
                let (updated_total_tool_calls, inflight_snapshot, tool_batch_summary) =
                    execute_tool_execution_plan(
                        ToolExecutionRuntimeInput {
                            store,
                            session_id: session_id.clone(),
                            thread_id: thread_id.clone(),
                            router,
                            events,
                            working_messages: &mut working_messages,
                            new_messages: &mut new_messages,
                        },
                        config,
                        plan,
                        total_tool_calls,
                        iteration,
                    )
                    .await?;
                total_tool_calls = updated_total_tool_calls;
                final_inflight_snapshot.extend(inflight_snapshot);
                let hook_decision = run_continuation_hook(
                    config,
                    ContinuationHookPhase::ToolBatchCompleted,
                    iteration,
                    LoopResult {
                        final_text: String::new(),
                        usage,
                        new_messages: new_messages.clone(),
                        iterations: iteration,
                        inflight_snapshot: final_inflight_snapshot.clone(),
                        requested_continuation: requested_continuation.clone(),
                        continuation_decision_trace: continuation_decision_trace.clone(),
                        next_tool_handle_sequence,
                    },
                    Some(tool_batch_summary),
                );
                continuation_decision_trace.push(trace_entry_for_hook_decision(
                    ContinuationHookPhase::ToolBatchCompleted,
                    &hook_decision,
                ));
                requested_continuation =
                    apply_hook_decision(requested_continuation.clone(), hook_decision);
            }
        }
    }

    crate::error::RuntimeSnafu {
        message: format!("max iterations exceeded: {}", config.max_iterations),
        stage: "agent-loop-max-iterations".to_string(),
        inflight_snapshot: (!final_inflight_snapshot.entries.is_empty())
            .then_some(final_inflight_snapshot),
    }
    .fail()
}

/// Publishes the final assistant text and packages the completed loop result.
#[allow(clippy::too_many_arguments)]
async fn finish_text_response<E>(
    store: &impl SessionStore,
    session_id: SessionId,
    thread_id: ThreadId,
    events: &E,
    message_id: Option<String>,
    text: String,
    usage: Usage,
    mut new_messages: Vec<Message>,
    iteration: usize,
    inflight_snapshot: ToolCallRuntimeSnapshot,
    requested_continuation: Option<SessionContinuationRequest>,
    continuation_decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
    next_tool_handle_sequence: usize,
) -> Result<LoopResult>
where
    E: EventSink,
{
    events
        .publish(AgentEvent::TextProduced { text: text.clone() })
        .await;
    let assistant = message_id
        .map(|id| Message::assistant_with_id(id, text.clone()))
        .unwrap_or_else(|| Message::assistant(text.clone()));
    store
        .append_message(session_id, thread_id, assistant.clone())
        .await?;
    new_messages.push(assistant);
    Ok(LoopResult {
        final_text: text,
        usage,
        new_messages,
        iterations: iteration,
        inflight_snapshot,
        requested_continuation,
        continuation_decision_trace,
        next_tool_handle_sequence,
    })
}

/// Runs the configured continuation hook for one runtime phase and returns its decision.
fn run_continuation_hook(
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
fn apply_hook_decision(
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
fn trace_entry_for_hook_decision(
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
