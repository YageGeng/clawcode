use llm::completion::Message;
use snafu::ResultExt;

use crate::events::EventSink;
use crate::{
    Result,
    context::{SessionTaskContext, TurnContext},
    error::SkillsSnafu,
    input::{user_inputs_to_messages, user_inputs_to_skill_inputs},
    model::AgentModel,
    prompt::{SystemPromptOverrides, build_system_prompt},
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
    pub(crate) prompt_overrides: SystemPromptOverrides,
    pub(crate) use_system_prompt_cache: bool,
    pub(crate) next_tool_handle_sequence: usize,
}

/// Executes one persisted turn using the supplied dependencies and runtime config.
pub(crate) async fn run_persisted_turn<M, E>(
    model: &M,
    store: &SessionTaskContext,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    collaboration_runtime: Option<tools::CollaborationRuntimeHandle>,
    turn_request: TurnExecutionRequest,
) -> Result<LoopResult>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    let TurnExecutionRequest {
        request,
        prompt_overrides,
        use_system_prompt_cache,
        next_tool_handle_sequence,
    } = turn_request;
    let mut history = store
        .load_messages_state(
            request.session_id,
            request.thread_id.clone(),
            config.recent_message_limit,
        )
        .await?;

    let user_messages = user_inputs_to_messages(&request.inputs);
    let persisted_user_message = user_messages
        .last()
        .cloned()
        .unwrap_or_else(|| Message::user(request.display_input.clone()));
    store
        .begin_turn_state(
            request.session_id,
            request.thread_id.clone(),
            request.display_input.clone(),
            persisted_user_message,
        )
        .await?;

    let skill_outcome = skills::SkillsManager::new(config.skills.clone())
        .load()
        .await;
    let system_prompt = resolve_system_prompt(
        store,
        router,
        &request,
        &prompt_overrides,
        &skill_outcome.skills,
        use_system_prompt_cache,
    )
    .await?;
    let turn_context =
        build_turn_context(store, &request, &prompt_overrides, system_prompt.clone()).await;
    let skill_inputs = user_inputs_to_skill_inputs(&request.inputs);
    let mention_options = skills::SkillMentionOptions::default();
    let selected_skills = skills::collect_explicit_skill_mentions(
        &skill_inputs,
        &skill_outcome.skills,
        &mention_options,
    );
    let skill_injections = skills::build_skill_injections(&selected_skills)
        .await
        .context(SkillsSnafu {
            stage: "runner-build-skill-injections".to_string(),
        })?;
    history.extend(skill_injections);
    history.extend(user_messages);

    let loop_result = run_loop_turn(
        model,
        store,
        router,
        events,
        config,
        AgentLoopRequest {
            session_id: request.session_id,
            thread_id: request.thread_id.clone(),
            system_prompt,
            turn_context,
            collaboration_runtime,
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

/// Resolves the effective system prompt, reusing a valid thread cache when allowed.
async fn resolve_system_prompt(
    store: &SessionTaskContext,
    router: &ToolRouter,
    request: &RunRequest,
    prompt_overrides: &SystemPromptOverrides,
    skills: &[skills::SkillMetadata],
    use_system_prompt_cache: bool,
) -> Result<Option<String>> {
    if use_system_prompt_cache
        && let Some(system_prompt) = store
            .read_cached_system_prompt(request.session_id, request.thread_id.clone())
            .await
    {
        return Ok(Some(system_prompt));
    }

    let system_prompt = build_system_prompt(router, skills, prompt_overrides)?;
    if use_system_prompt_cache && let Some(prompt) = system_prompt.clone() {
        store
            .save_cached_system_prompt(request.session_id, request.thread_id.clone(), prompt)
            .await;
    }
    Ok(system_prompt)
}

/// Rebuilds the effective turn context so tool execution and final persistence share one identity.
async fn build_turn_context(
    store: &SessionTaskContext,
    request: &RunRequest,
    prompt_overrides: &SystemPromptOverrides,
    system_prompt: Option<String>,
) -> TurnContext {
    let mut turn_context = store
        .load_turn_context(request.session_id, request.thread_id.clone())
        .await
        .unwrap_or_else(|| TurnContext::new(request.session_id, request.thread_id.clone()));

    turn_context.system_prompt = system_prompt;
    if let Some(cwd) = prompt_overrides.cwd.as_ref() {
        turn_context.cwd = Some(cwd.to_string_lossy().to_string());
    }
    if let Some(current_date) = prompt_overrides.current_date.clone() {
        turn_context.current_date = Some(current_date);
    }
    turn_context
}
