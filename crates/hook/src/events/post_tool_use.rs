use std::path::PathBuf;

use protocol::{HookEventName, HookOutputEntry, HookRunStatus, HookRunSummary};

use crate::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser::{
    additional_context, block_reason, join_feedback_messages,
    parse_json_object, top_level_reason, validate_post_tool_use_output,
};
use crate::events::common::matcher_inputs;

/// Request payload for `PostToolUse` hooks.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct PostToolUseRequest {
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
    /// Hook-facing tool response.
    pub tool_response: serde_json::Value,
}

impl PostToolUseRequest {
    /// Serialize this request for command stdin.
    pub(crate) fn input_json(&self) -> String {
        serde_json::json!({
            "session_id": self.session_id.to_string(),
            "turn_id": String::from(&self.turn_id),
            "transcript_path": self.transcript_path.as_ref().map(|path| path.display().to_string()),
            "cwd": self.cwd.display().to_string(),
            "hook_event_name": "PostToolUse",
            "model": self.model,
            "permission_mode": self.permission_mode,
            "tool_name": self.tool_name,
            "tool_input": self.tool_input,
            "tool_response": self.tool_response,
            "tool_use_id": self.tool_use_id,
        })
        .to_string()
    }
}

/// Folded `PostToolUse` outcome.
#[derive(Debug, Clone, Default, PartialEq, typed_builder::TypedBuilder)]
pub struct PostToolUseOutcome {
    /// Completed hook events.
    #[builder(default)]
    pub hook_events: Vec<protocol::HookCompletedEvent>,
    /// Whether a hook requested the turn to stop.
    pub should_stop: bool,
    /// Optional stop reason.
    #[builder(default)]
    pub stop_reason: Option<String>,
    /// Additional model context entries.
    #[builder(default)]
    pub additional_contexts: Vec<String>,
    /// Optional feedback that replaces model-visible tool output.
    #[builder(default)]
    pub feedback_message: Option<String>,
}

/// Parsed data from one `PostToolUse` handler.
#[derive(Debug, Clone, Default, PartialEq, typed_builder::TypedBuilder)]
pub(crate) struct PostToolUseHandlerResult {
    /// Whether this handler requested stop.
    pub(crate) should_stop: bool,
    /// Optional stop reason.
    #[builder(default)]
    pub(crate) stop_reason: Option<String>,
    /// Additional model context entries.
    #[builder(default)]
    pub(crate) additional_contexts: Vec<String>,
    /// Optional replacement feedback.
    #[builder(default)]
    pub(crate) feedback_message: Option<String>,
}

/// Return preview summaries for matching `PostToolUse` hooks.
pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    request: &PostToolUseRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers_for_matcher_inputs(
        handlers,
        HookEventName::PostToolUse,
        &matcher_inputs(&request.tool_name, &request.matcher_aliases),
    )
    .into_iter()
    .map(|handler| dispatcher::running_summary(&handler))
    .collect()
}

/// Run matching `PostToolUse` hooks and fold their outcomes.
pub(crate) async fn run(
    handlers: &[ConfiguredHandler],
    request: PostToolUseRequest,
) -> PostToolUseOutcome {
    let matched = dispatcher::select_handlers_for_matcher_inputs(
        handlers,
        HookEventName::PostToolUse,
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
    let mut outcome = PostToolUseOutcome::default();
    for result in results {
        outcome.hook_events.push(result.completed);
        outcome
            .additional_contexts
            .extend(result.data.additional_contexts);
        if result.data.should_stop {
            outcome.should_stop = true;
            outcome.stop_reason =
                outcome.stop_reason.or(result.data.stop_reason);
        }
        if result.data.feedback_message.is_some() {
            outcome.feedback_message = join_feedback_messages(
                outcome.feedback_message,
                result.data.feedback_message,
            );
        }
    }
    outcome
}

/// Parse one completed `PostToolUse` command run.
fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: protocol::TurnId,
) -> dispatcher::ParsedHandler<PostToolUseHandlerResult> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut data = PostToolUseHandlerResult::builder()
        .should_stop(false)
        .build();
    if let Some(error) = run_result.error.as_ref() {
        status = HookRunStatus::Failed;
        entries.push(HookOutputEntry::error(error));
    } else if run_result.exit_code == Some(2) {
        if run_result.stderr.trim().is_empty() {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry::error(
                "PostToolUse hook exited with code 2 but did not write feedback to stderr",
            ));
        } else {
            let reason = run_result.stderr.trim().to_string();
            data.feedback_message = Some(reason.clone());
            entries.push(HookOutputEntry::feedback(reason));
        }
    } else if run_result.exit_code != Some(0) {
        status = HookRunStatus::Failed;
        entries.push(HookOutputEntry::error(format!(
            "hook exited with code {}",
            run_result.exit_code.unwrap_or(-1)
        )));
    } else {
        parse_post_tool_use_stdout(
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

/// Parse successful `PostToolUse` stdout into status entries and handler data.
fn parse_post_tool_use_stdout(
    entries: &mut Vec<HookOutputEntry>,
    status: &mut HookRunStatus,
    data: &mut PostToolUseHandlerResult,
    stdout: &str,
) {
    match parse_json_object(stdout) {
        Ok(None) => {}
        Err(error) => {
            *status = HookRunStatus::Failed;
            entries.push(HookOutputEntry::error(error));
        }
        Ok(Some(value)) => {
            if let Some(error) = validate_post_tool_use_output(&value) {
                *status = HookRunStatus::Failed;
                entries.push(HookOutputEntry::error(error));
            } else if value.get("continue").and_then(serde_json::Value::as_bool)
                == Some(false)
            {
                apply_post_tool_use_stop(entries, status, data, &value);
            } else {
                apply_post_tool_use_json(entries, status, data, &value);
            }
        }
    }
}

/// Apply a validated `PostToolUse` stop output to status entries and handler data.
fn apply_post_tool_use_stop(
    entries: &mut Vec<HookOutputEntry>,
    status: &mut HookRunStatus,
    data: &mut PostToolUseHandlerResult,
    value: &serde_json::Value,
) {
    *status = HookRunStatus::Stopped;
    data.should_stop = true;
    data.stop_reason = value
        .get("stopReason")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let stop_text = data
        .stop_reason
        .clone()
        .unwrap_or_else(|| "PostToolUse hook stopped execution".to_string());
    entries.push(HookOutputEntry::stop(stop_text.clone()));
    data.feedback_message = top_level_reason(value).or(Some(stop_text));
}

/// Apply validated `PostToolUse` JSON output to status entries and handler data.
fn apply_post_tool_use_json(
    entries: &mut Vec<HookOutputEntry>,
    status: &mut HookRunStatus,
    data: &mut PostToolUseHandlerResult,
    value: &serde_json::Value,
) {
    if let Some(context) = additional_context(value) {
        data.additional_contexts.push(context.clone());
        entries.push(HookOutputEntry::context(context));
    }
    if let Some(reason) = block_reason(value) {
        *status = HookRunStatus::Blocked;
        data.feedback_message = Some(reason.clone());
        entries.push(HookOutputEntry::feedback(reason));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use protocol::{HookEventName, HookRunStatus, HookSource};

    use super::*;

    /// JSON-looking PostToolUse stdout that cannot be parsed is surfaced as a hook failure.
    #[test]
    fn malformed_json_fails_hook() {
        let parsed = parse_completed(
            &test_handler(HookEventName::PostToolUse, "echo", 0),
            command_result(Some(0), "{", ""),
            protocol::TurnId::from("turn-1"),
        );

        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert_eq!(parsed.data, PostToolUseHandlerResult::default());
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
