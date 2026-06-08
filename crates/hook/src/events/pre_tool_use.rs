use std::path::PathBuf;

use protocol::{HookEventName, HookOutputEntry, HookRunStatus, HookRunSummary};

use crate::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser::{
    additional_context, parse_json_object, pre_tool_use_deny_reason,
    validate_pre_tool_use_output,
};
use crate::events::common::matcher_inputs;

/// Request payload for `PreToolUse` hooks.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct PreToolUseRequest {
    /// Session id attached to the hook input.
    pub session_id: protocol::SessionId,
    /// Turn id attached to the hook input.
    pub turn_id: protocol::TurnId,
    /// Optional transcript path.
    #[builder(default, setter(strip_option))]
    pub transcript_path: Option<PathBuf>,
    /// Working directory for command execution.
    pub cwd: PathBuf,
    /// Current model id.
    pub model: String,
    /// Current permission mode label.
    pub permission_mode: String,
    /// Canonical tool name.
    pub tool_name: String,
    /// Compatibility aliases used only for matcher selection.
    #[builder(default)]
    pub matcher_aliases: Vec<String>,
    /// Tool use id.
    pub tool_use_id: String,
    /// Hook-facing tool input.
    pub tool_input: serde_json::Value,
}

impl PreToolUseRequest {
    /// Serialize this request for command stdin.
    pub(crate) fn input_json(&self) -> String {
        serde_json::json!({
            "session_id": self.session_id.to_string(),
            "turn_id": String::from(&self.turn_id),
            "transcript_path": self.transcript_path.as_ref().map(|path| path.display().to_string()),
            "cwd": self.cwd.display().to_string(),
            "hook_event_name": "PreToolUse",
            "model": self.model,
            "permission_mode": self.permission_mode,
            "tool_name": self.tool_name,
            "tool_input": self.tool_input,
            "tool_use_id": self.tool_use_id,
        })
        .to_string()
    }
}

/// Folded `PreToolUse` outcome.
#[derive(Debug, Clone, Default, PartialEq, typed_builder::TypedBuilder)]
pub struct PreToolUseOutcome {
    /// Completed hook events.
    #[builder(default)]
    pub hook_events: Vec<protocol::HookCompletedEvent>,
    /// Whether any hook blocked the tool.
    pub should_block: bool,
    /// First block reason in declaration order.
    #[builder(default)]
    pub block_reason: Option<String>,
    /// Additional model context entries.
    #[builder(default)]
    pub additional_contexts: Vec<String>,
    /// Latest input rewrite by completion order.
    #[builder(default)]
    pub updated_input: Option<serde_json::Value>,
}

/// Parsed data from one `PreToolUse` handler.
#[derive(Debug, Clone, Default, PartialEq, typed_builder::TypedBuilder)]
pub struct PreToolUseHandlerResult {
    /// Whether this handler blocked execution.
    pub should_block: bool,
    /// Optional block reason.
    #[builder(default)]
    pub block_reason: Option<String>,
    /// Additional model context entries.
    #[builder(default)]
    pub additional_contexts: Vec<String>,
    /// Optional tool input rewrite.
    #[builder(default)]
    pub updated_input: Option<serde_json::Value>,
    /// Completion order used to resolve competing rewrites.
    #[builder(default)]
    pub completion_order: usize,
}

/// Return preview summaries for matching `PreToolUse` hooks.
pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    request: &PreToolUseRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers_for_matcher_inputs(
        handlers,
        HookEventName::PreToolUse,
        &matcher_inputs(&request.tool_name, &request.matcher_aliases),
    )
    .into_iter()
    .map(|handler| dispatcher::running_summary(&handler))
    .collect()
}

/// Run matching `PreToolUse` hooks and fold their outcomes.
pub(crate) async fn run(
    handlers: &[ConfiguredHandler],
    request: PreToolUseRequest,
) -> PreToolUseOutcome {
    let matched = dispatcher::select_handlers_for_matcher_inputs(
        handlers,
        HookEventName::PreToolUse,
        &matcher_inputs(&request.tool_name, &request.matcher_aliases),
    );
    let results = dispatcher::execute_handlers(
        matched,
        request.input_json(),
        request.cwd.clone(),
        request.turn_id.clone(),
        parse_completed,
    )
    .await;
    let mut hook_events = Vec::new();
    let mut handler_results = Vec::new();
    for mut result in results {
        result.data.completion_order = result.completion_order;
        hook_events.push(result.completed);
        handler_results.push(result.data);
    }
    let mut outcome = fold_pre_tool_use_results(handler_results);
    outcome.hook_events = hook_events;
    outcome
}

/// Fold per-handler `PreToolUse` results into the event outcome.
#[must_use]
pub fn fold_pre_tool_use_results(
    results: Vec<PreToolUseHandlerResult>,
) -> PreToolUseOutcome {
    let should_block = results.iter().any(|result| result.should_block);
    let block_reason = results
        .iter()
        .find_map(|result| result.block_reason.clone());
    let additional_contexts = results
        .iter()
        .flat_map(|result| result.additional_contexts.clone())
        .collect();
    let updated_input = if should_block {
        None
    } else {
        results
            .iter()
            .filter_map(|result| {
                result
                    .updated_input
                    .clone()
                    .map(|input| (result.completion_order, input))
            })
            .max_by_key(|(completion_order, _)| *completion_order)
            .map(|(_, input)| input)
    };

    PreToolUseOutcome::builder()
        .should_block(should_block)
        .block_reason(block_reason)
        .additional_contexts(additional_contexts)
        .updated_input(updated_input)
        .build()
}

/// Parse one completed `PreToolUse` command run.
fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: protocol::TurnId,
) -> dispatcher::ParsedHandler<PreToolUseHandlerResult> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut data = PreToolUseHandlerResult::builder()
        .should_block(false)
        .build();
    if let Some(error) = run_result.error.as_ref() {
        status = HookRunStatus::Failed;
        entries.push(HookOutputEntry::error(error));
    } else if run_result.exit_code == Some(2) {
        if run_result.stderr.trim().is_empty() {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry::error(
                "PreToolUse hook exited with code 2 but did not write a blocking reason to stderr",
            ));
        } else {
            status = HookRunStatus::Blocked;
            data.should_block = true;
            data.block_reason = Some(run_result.stderr.trim().to_string());
            entries.push(HookOutputEntry::feedback(run_result.stderr.trim()));
        }
    } else if run_result.exit_code != Some(0) {
        status = HookRunStatus::Failed;
        entries.push(HookOutputEntry::error(format!(
            "hook exited with code {}",
            run_result.exit_code.unwrap_or(-1)
        )));
    } else {
        parse_pre_tool_use_stdout(
            &mut entries,
            &mut status,
            &mut data,
            &run_result.stdout,
        );
    }
    let completed = dispatcher::completed_event(
        handler,
        &run_result,
        turn_id,
        status,
        entries,
    );

    dispatcher::ParsedHandler {
        completed,
        data,
        completion_order: 0,
    }
}

/// Parse successful `PreToolUse` stdout into status entries and handler data.
fn parse_pre_tool_use_stdout(
    entries: &mut Vec<HookOutputEntry>,
    status: &mut HookRunStatus,
    data: &mut PreToolUseHandlerResult,
    stdout: &str,
) {
    match parse_json_object(stdout) {
        Ok(None) => {}
        Err(error) => {
            *status = HookRunStatus::Failed;
            entries.push(HookOutputEntry::error(error));
        }
        Ok(Some(value)) => {
            if let Some(error) = validate_pre_tool_use_output(&value) {
                *status = HookRunStatus::Failed;
                entries.push(HookOutputEntry::error(error));
            } else {
                apply_pre_tool_use_json(entries, status, data, &value);
            }
        }
    }
}

/// Apply validated `PreToolUse` JSON output to status entries and handler data.
fn apply_pre_tool_use_json(
    entries: &mut Vec<HookOutputEntry>,
    status: &mut HookRunStatus,
    data: &mut PreToolUseHandlerResult,
    value: &serde_json::Value,
) {
    if let Some(context) = additional_context(value) {
        data.additional_contexts.push(context.clone());
        entries.push(HookOutputEntry::context(context));
    }
    if let Some(reason) = pre_tool_use_deny_reason(value) {
        *status = HookRunStatus::Blocked;
        data.should_block = true;
        data.block_reason = Some(reason.clone());
        entries.push(HookOutputEntry::feedback(reason));
    } else {
        data.updated_input = value
            .get("hookSpecificOutput")
            .and_then(|output| output.get("updatedInput"))
            .cloned();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use protocol::{HookEventName, HookRunStatus, HookSource};

    use super::*;

    /// PreToolUse rejects updatedInput unless permissionDecision is explicitly allow.
    #[test]
    fn rejects_updated_input_without_allow() {
        let parsed = parse_completed(
            &test_handler(HookEventName::PreToolUse, "echo", 0),
            command_result(
                Some(0),
                r#"{"hookSpecificOutput":{"updatedInput":{"command":"pwd"}}}"#,
                "",
            ),
            protocol::TurnId::from("turn-1"),
        );

        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert!(parsed.data.updated_input.is_none());
    }

    /// JSON-looking PreToolUse stdout that cannot be parsed is surfaced as a hook failure.
    #[test]
    fn malformed_json_fails_hook() {
        let parsed = parse_completed(
            &test_handler(HookEventName::PreToolUse, "echo", 0),
            command_result(Some(0), "{", ""),
            protocol::TurnId::from("turn-1"),
        );

        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert!(parsed.data.updated_input.is_none());
    }

    /// Build one configured handler for parser tests.
    fn test_handler(
        event_name: HookEventName,
        command: &str,
        display_order: i64,
    ) -> ConfiguredHandler {
        ConfiguredHandler::builder()
            .event_name(event_name)
            .command(command.to_string())
            .timeout_sec(2)
            .source_path(PathBuf::from("/tmp/hooks.json"))
            .source(HookSource::Project)
            .display_order(display_order)
            .build()
    }

    /// Build a completed command result for parser tests.
    fn command_result(
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> CommandRunResult {
        CommandRunResult {
            started_at: 1,
            completed_at: 2,
            duration_ms: 1,
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            error: None,
        }
    }
}
