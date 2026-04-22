use llm::completion::Message;

use crate::events::EventSink;
use crate::{
    Result,
    context::SessionTaskContext,
    model::AgentModel,
    runtime::{
        continuation::AgentLoopConfig,
        task::{RunRequest, preserve_original_error_after_task_cleanup},
        turn::{AgentLoopRequest, LoopResult, run_turn as run_loop_turn},
    },
    tools::router::ToolRouter,
};

/// Internal turn request that carries the persisted run request plus task-scoped loop state.
#[derive(Debug, Clone)]
pub(crate) struct TurnExecutionRequest {
    pub(crate) request: RunRequest,
    pub(crate) system_prompt: Option<String>,
    pub(crate) next_tool_handle_sequence: usize,
}

/// Executes one persisted turn using the supplied dependencies and runtime config.
pub(crate) async fn run_persisted_turn<M, E>(
    model: &M,
    store: &SessionTaskContext,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    turn_request: TurnExecutionRequest,
) -> Result<LoopResult>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    let TurnExecutionRequest {
        request,
        system_prompt,
        next_tool_handle_sequence,
    } = turn_request;
    let mut history = store
        .load_messages_state(
            request.session_id.clone(),
            request.thread_id.clone(),
            config.recent_message_limit,
        )
        .await?;

    let user_message = Message::user(request.input.clone());
    store
        .begin_turn_state(
            request.session_id.clone(),
            request.thread_id.clone(),
            request.input.clone(),
            user_message.clone(),
        )
        .await?;
    history.push(user_message);

    let loop_result = run_loop_turn(
        model,
        store,
        router,
        events,
        config,
        AgentLoopRequest {
            session_id: request.session_id.clone(),
            thread_id: request.thread_id.clone(),
            system_prompt,
            working_messages: history,
            next_tool_handle_sequence,
        },
    )
    .await;

    match loop_result {
        Ok(loop_result) => Ok(loop_result),
        Err(error) => Err(preserve_original_error_after_task_cleanup(store, &request, error).await),
    }
}
