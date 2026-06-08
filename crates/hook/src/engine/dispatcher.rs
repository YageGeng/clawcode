use std::path::PathBuf;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use protocol::{
    HookCompletedEvent, HookEventName, HookExecutionMode, HookHandlerType,
    HookOutputEntry, HookRunStatus, HookRunSummary, HookScope,
};

use super::ConfiguredHandler;
use super::command_runner::{CommandRunResult, run_command};
use crate::events::common::matches_matcher;

/// Parsed command handler output plus its wall-clock completion order.
#[derive(Debug)]
pub(crate) struct ParsedHandler<T> {
    /// Protocol event generated for this completed handler.
    pub(crate) completed: HookCompletedEvent,
    /// Event-specific parsed data.
    pub(crate) data: T,
    /// Order in which the command actually completed.
    pub(crate) completion_order: usize,
}

/// Select matching handlers once even when several aliases match.
pub(crate) fn select_handlers_for_matcher_inputs(
    handlers: &[ConfiguredHandler],
    event_name: HookEventName,
    matcher_inputs: &[&str],
) -> Vec<ConfiguredHandler> {
    // Check each configured handler once, even when several compatibility names
    // match the same regex. A hook like `shell|Bash` should run once for a tool
    // call, not once per matching alias.
    handlers
        .iter()
        .filter(|handler| handler.event_name == event_name)
        .filter(|handler| {
            if matcher_inputs.is_empty() {
                matches_matcher(handler.matcher.as_deref(), None)
            } else {
                matcher_inputs.iter().any(|input| {
                    matches_matcher(handler.matcher.as_deref(), Some(input))
                })
            }
        })
        .cloned()
        .collect()
}

/// Build a running summary for one selected hook.
pub(crate) fn running_summary(handler: &ConfiguredHandler) -> HookRunSummary {
    HookRunSummary::builder()
        .id(handler.run_id())
        .event_name(handler.event_name)
        .handler_type(HookHandlerType::Command)
        .execution_mode(HookExecutionMode::Sync)
        .scope(scope_for_event(handler.event_name))
        .source_path(handler.source_path.display().to_string())
        .source(handler.source)
        .display_order(handler.display_order)
        .status(HookRunStatus::Running)
        .status_message(handler.status_message.clone())
        .started_at(chrono::Utc::now().timestamp())
        .entries(Vec::<HookOutputEntry>::new())
        .build()
}

/// Execute selected command hooks concurrently while returning them in configuration order.
pub(crate) async fn execute_handlers<T>(
    handlers: Vec<ConfiguredHandler>,
    input_json: String,
    cwd: PathBuf,
    turn_id: protocol::TurnId,
    parse: fn(
        &ConfiguredHandler,
        CommandRunResult,
        protocol::TurnId,
    ) -> ParsedHandler<T>,
) -> Vec<ParsedHandler<T>> {
    let mut pending = FuturesUnordered::new();
    for (configured_order, handler) in handlers.into_iter().enumerate() {
        let input_json = input_json.clone();
        let cwd = cwd.clone();
        let turn_id = turn_id.clone();
        pending.push(async move {
            let result = run_command(&handler, &input_json, cwd).await;
            (configured_order, parse(&handler, result, turn_id))
        });
    }

    let mut completed = Vec::new();
    let mut completion_order = 0;
    while let Some((configured_order, mut parsed)) = pending.next().await {
        parsed.completion_order = completion_order;
        completion_order += 1;
        completed.push((configured_order, parsed));
    }
    completed.sort_by_key(|(configured_order, _)| *configured_order);
    completed.into_iter().map(|(_, parsed)| parsed).collect()
}

/// Build a completed protocol event from parsed hook data.
pub(crate) fn completed_event(
    handler: &ConfiguredHandler,
    run_result: &CommandRunResult,
    turn_id: protocol::TurnId,
    status: HookRunStatus,
    entries: Vec<HookOutputEntry>,
) -> HookCompletedEvent {
    HookCompletedEvent::builder()
        .turn_id(turn_id)
        .run(completed_summary(handler, run_result, status, entries))
        .build()
}

/// Build a completed run summary for one finished hook command.
fn completed_summary(
    handler: &ConfiguredHandler,
    run_result: &CommandRunResult,
    status: HookRunStatus,
    entries: Vec<HookOutputEntry>,
) -> HookRunSummary {
    HookRunSummary::builder()
        .id(handler.run_id())
        .event_name(handler.event_name)
        .handler_type(HookHandlerType::Command)
        .execution_mode(HookExecutionMode::Sync)
        .scope(scope_for_event(handler.event_name))
        .source_path(handler.source_path.display().to_string())
        .source(handler.source)
        .display_order(handler.display_order)
        .status(status)
        .status_message(handler.status_message.clone())
        .started_at(run_result.started_at)
        .completed_at(Some(run_result.completed_at))
        .duration_ms(Some(run_result.duration_ms))
        .entries(entries)
        .build()
}

/// Return the display scope for a hook lifecycle event.
fn scope_for_event(event_name: HookEventName) -> HookScope {
    match event_name {
        HookEventName::SessionStart | HookEventName::SubagentStart => {
            HookScope::Thread
        }
        HookEventName::PreToolUse
        | HookEventName::PermissionRequest
        | HookEventName::PostToolUse
        | HookEventName::PreCompact
        | HookEventName::PostCompact
        | HookEventName::UserPromptSubmit
        | HookEventName::SubagentStop
        | HookEventName::Stop => HookScope::Turn,
    }
}
