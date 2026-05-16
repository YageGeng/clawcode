//! Renderable TUI state reduced from ACP session updates.

use std::collections::HashMap;
use std::path::PathBuf;

use agent_client_protocol::schema::{
    ContentBlock, SessionId, SessionNotification, SessionUpdate, StopReason, ToolCall,
    ToolCallContent, ToolCallId, ToolCallStatus, ToolCallUpdate, UsageUpdate,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::approval::PendingApproval;

/// Renderable transcript cell stored in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptCell {
    /// Assistant answer text.
    Assistant(String),
    /// Assistant reasoning text.
    Reasoning(String),
    /// User prompt text.
    User(String),
    /// System or runtime message text.
    System(String),
    /// ACP tool invocation and its live output state.
    ToolCall(ToolCallView),
}

impl TranscriptCell {
    /// Returns the cell text regardless of the transcript role.
    pub fn text(&self) -> &str {
        match self {
            TranscriptCell::Assistant(text)
            | TranscriptCell::Reasoning(text)
            | TranscriptCell::User(text)
            | TranscriptCell::System(text) => text,
            TranscriptCell::ToolCall(tool) => tool.output(),
        }
    }
}

/// Renderable view of an ACP tool call.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ToolCallView {
    /// Unique ACP call id for the tool invocation.
    call_id: String,
    /// Tool title shown to the user.
    name: String,
    /// JSON argument text accumulated from ACP raw input.
    arguments: String,
    /// Tool output text accumulated from ACP update content.
    output: String,
    /// Latest ACP execution status for the tool.
    status: ToolCallStatus,
}

impl ToolCallView {
    /// Returns the ACP call id.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Returns the display tool name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the accumulated argument text.
    pub fn arguments(&self) -> &str {
        &self.arguments
    }

    /// Returns the accumulated output text.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Returns the latest tool execution status.
    pub fn status(&self) -> ToolCallStatus {
        self.status
    }
}

/// Token usage totals for the current turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageView {
    /// Prompt/input tokens used by the current turn.
    input_tokens: u64,
    /// Completion/output tokens used by the current turn.
    output_tokens: u64,
    /// Combined prompt and completion token count.
    total_tokens: u64,
}

impl UsageView {
    /// Builds a usage view and precomputes the total for status rendering.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
        }
    }

    /// Builds a usage view when ACP only provides a total token count.
    pub fn from_total(total_tokens: u64) -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens,
        }
    }

    /// Returns prompt/input tokens.
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// Returns completion/output tokens.
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Returns total token usage.
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }
}

/// Renderable application state for one ACP session.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct AppState {
    /// ACP session represented by this TUI state.
    session_id: SessionId,
    /// Working directory associated with the session.
    cwd: PathBuf,
    /// Human-readable provider/model label.
    model_label: String,
    /// Ordered transcript cells ready for rendering.
    #[builder(default)]
    transcript: Vec<TranscriptCell>,
    /// Transcript index for each tool call id.
    #[builder(default)]
    tool_call_indices: HashMap<String, usize>,
    /// Latest token usage update.
    #[builder(default)]
    usage: UsageView,
    /// Approval request currently waiting for user input.
    #[builder(default, setter(strip_option))]
    pending_approval: Option<PendingApproval>,
    /// True while a submitted user prompt is still running.
    #[builder(default)]
    running_prompt: bool,
    /// Stop reason recorded from the last completed turn.
    #[builder(default, setter(strip_option))]
    last_stop_reason: Option<StopReason>,
    /// Last runtime error message shown to the user.
    #[builder(default, setter(strip_option))]
    last_error: Option<String>,
}

impl AppState {
    /// Creates renderable state for a new TUI ACP session.
    pub fn new(session_id: SessionId, cwd: PathBuf, model_label: String) -> Self {
        AppState::builder()
            .session_id(session_id)
            .cwd(cwd)
            .model_label(model_label)
            .build()
    }

    /// Returns the ACP session id represented by this state.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the session working directory.
    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    /// Returns the provider/model label used in status lines.
    pub fn model_label(&self) -> &str {
        &self.model_label
    }

    /// Returns the renderable transcript cells.
    pub fn transcript(&self) -> &[TranscriptCell] {
        &self.transcript
    }

    /// Returns the latest token usage view.
    pub fn usage(&self) -> UsageView {
        self.usage
    }

    /// Applies one ACP session notification to the renderable state.
    pub fn apply_session_update(&mut self, notification: SessionNotification) {
        if notification.session_id != self.session_id {
            return;
        }

        match notification.update {
            SessionUpdate::UserMessageChunk(chunk) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    self.transcript.push(TranscriptCell::User(text));
                }
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    Self::append_to_last_or_push(
                        &mut self.transcript,
                        text,
                        TranscriptRole::Assistant,
                    );
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    Self::append_to_last_or_push(
                        &mut self.transcript,
                        text,
                        TranscriptRole::Reasoning,
                    );
                }
            }
            SessionUpdate::ToolCall(tool_call) => self.apply_tool_call(tool_call),
            SessionUpdate::ToolCallUpdate(update) => self.apply_tool_call_update(update),
            SessionUpdate::Plan(_) => {}
            SessionUpdate::UsageUpdate(update) => self.apply_usage_update(update),
            _ => {}
        }
    }

    /// Appends a user prompt and marks the runtime as waiting for a turn result.
    pub fn append_user_message(&mut self, text: impl Into<String>) {
        self.transcript.push(TranscriptCell::User(text.into()));
        self.running_prompt = true;
        self.last_error = None;
        self.last_stop_reason = None;
        self.pending_approval = None;
    }

    /// Records a prompt completion returned by ACP.
    pub fn finish_prompt(&mut self, stop_reason: StopReason) {
        self.running_prompt = false;
        self.last_stop_reason = Some(stop_reason);
        self.pending_approval = None;
    }

    /// Records an ACP permission request in renderable form.
    pub fn set_pending_approval(&mut self, approval: PendingApproval) {
        self.pending_approval = Some(approval);
    }

    /// Records an error message in both state and the visible transcript.
    pub fn set_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.last_error = Some(message.clone());
        self.transcript.push(TranscriptCell::System(message));
        self.running_prompt = false;
        self.pending_approval = None;
    }

    /// Builds a compact status line suitable for the top TUI chrome.
    pub fn top_status_line(&self) -> String {
        if let Some(error) = &self.last_error {
            return format!("error: {error}");
        }
        if self.pending_approval.is_some() {
            return "waiting for approval".to_string();
        }
        if self.running_prompt {
            return "running".to_string();
        }
        if let Some(stop_reason) = self.last_stop_reason {
            return format!("stopped: {stop_reason:?}");
        }
        "idle".to_string()
    }

    /// Builds the bottom status line with model, cwd, and token usage.
    pub fn bottom_status_line(&self, width: usize) -> String {
        let token_status = format!("tokens: {}", self.usage.total_tokens());
        let token_width = UnicodeWidthStr::width(token_status.as_str());
        if width <= token_width {
            return Self::truncate_to_display_width(&token_status, width);
        }

        let prefix = format!("{} | {}", self.model_label, self.cwd.display());
        let separator = " | ";
        let suffix_width = UnicodeWidthStr::width(separator) + token_width;
        if width > suffix_width {
            let prefix_width = width - suffix_width;
            // Preserve the token segment by truncating the less critical model/cwd prefix first.
            let status = format!(
                "{}{separator}{token_status}",
                Self::truncate_to_display_width(&prefix, prefix_width)
            );
            return Self::truncate_to_display_width(&status, width);
        }

        let status = format!("{separator}{token_status}");
        Self::truncate_to_display_width(&status, width)
    }

    /// Takes and clears the pending approval request.
    pub fn take_pending_approval(&mut self) -> Option<PendingApproval> {
        self.pending_approval.take()
    }

    /// Returns the pending approval request without clearing it.
    pub fn pending_approval(&self) -> Option<&PendingApproval> {
        self.pending_approval.as_ref()
    }

    /// Clears the pending approval request without returning it.
    pub fn clear_pending_approval(&mut self) {
        self.pending_approval = None;
    }

    /// Returns whether the last submitted user prompt is still running.
    pub fn is_running_prompt(&self) -> bool {
        self.running_prompt
    }

    /// Returns the stop reason from the last completed turn.
    pub fn last_stop_reason(&self) -> Option<StopReason> {
        self.last_stop_reason
    }

    /// Returns the last recorded runtime error.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Applies a full ACP tool-call snapshot.
    fn apply_tool_call(&mut self, tool_call: ToolCall) {
        let call_id = tool_call_id_string(&tool_call.tool_call_id);
        let title = tool_call.title;
        let status = tool_call.status;
        let raw_input = tool_call
            .raw_input
            .as_ref()
            .map(json_to_display)
            .unwrap_or_default();
        let output = tool_call
            .content
            .iter()
            .filter_map(tool_content_text)
            .collect::<Vec<_>>()
            .join("\n");

        let entry = self.pending_tool_call_view(call_id.clone());
        entry.name = title;
        entry.arguments = raw_input;
        entry.status = status;
        if !output.is_empty() {
            entry.output.push_str(&output);
        }
    }

    /// Applies an ACP tool-call field update to an existing or placeholder view.
    fn apply_tool_call_update(&mut self, update: ToolCallUpdate) {
        let call_id = tool_call_id_string(&update.tool_call_id);
        {
            let entry = self.pending_tool_call_view(call_id.clone());
            if let Some(title) = update.fields.title {
                entry.name = title;
            }
            if let Some(raw_input) = update.fields.raw_input {
                entry.arguments = json_to_display(&raw_input);
            }
            if let Some(raw_output) = update.fields.raw_output {
                entry.output.push_str(&json_to_display(&raw_output));
            }
            if let Some(content) = update.fields.content {
                let text = content
                    .iter()
                    .filter_map(tool_content_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                entry.output.push_str(&text);
            }
            if let Some(status) = update.fields.status {
                entry.status = status;
            }
        }
    }

    /// Applies an ACP token usage update.
    fn apply_usage_update(&mut self, update: UsageUpdate) {
        self.usage = UsageView::from_total(update.used);
    }

    /// Returns an existing tool view or creates a pending placeholder for updates.
    fn pending_tool_call_view(&mut self, call_id: String) -> &mut ToolCallView {
        let index = if let Some(index) = self.tool_call_indices.get(&call_id) {
            *index
        } else {
            let index = self.transcript.len();
            self.transcript
                .push(TranscriptCell::ToolCall(empty_tool_call_view(
                    call_id.clone(),
                )));
            self.tool_call_indices.insert(call_id.clone(), index);
            index
        };

        let Some(cell) = self.transcript.get_mut(index) else {
            unreachable!("tool call index must point inside transcript");
        };
        match cell {
            TranscriptCell::ToolCall(tool) => tool,
            _ => unreachable!("tool call index must point at a tool call transcript cell"),
        }
    }

    /// Appends text to the previous compatible cell or pushes a new cell.
    fn append_to_last_or_push(
        transcript: &mut Vec<TranscriptCell>,
        text: String,
        role: TranscriptRole,
    ) {
        // Coalescing adjacent chunks keeps rendering stable while preserving role boundaries.
        match (transcript.last_mut(), role) {
            (Some(TranscriptCell::Assistant(existing)), TranscriptRole::Assistant) => {
                existing.push_str(&text);
            }
            (Some(TranscriptCell::Reasoning(existing)), TranscriptRole::Reasoning) => {
                existing.push_str(&text);
            }
            (_, TranscriptRole::Assistant) => transcript.push(TranscriptCell::Assistant(text)),
            (_, TranscriptRole::Reasoning) => transcript.push(TranscriptCell::Reasoning(text)),
        }
    }

    /// Returns text truncated to the requested terminal display width.
    fn truncate_to_display_width(text: &str, max_width: usize) -> String {
        if max_width == 0 {
            return String::new();
        }
        if UnicodeWidthStr::width(text) <= max_width {
            return text.to_string();
        }
        if max_width <= 3 {
            return ".".repeat(max_width);
        }

        let mut truncated = String::new();
        let mut used_width = 0;
        let content_width = max_width - 3;
        for ch in text.chars() {
            let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            // Stop before the next character would overflow the space reserved before "...".
            if used_width + char_width > content_width {
                break;
            }
            truncated.push(ch);
            used_width += char_width;
        }
        truncated.push_str("...");
        truncated
    }
}

/// Builds the placeholder tool call used when updates arrive before snapshots.
fn empty_tool_call_view(call_id: String) -> ToolCallView {
    ToolCallView::builder()
        .call_id(call_id)
        .name(String::new())
        .arguments(String::new())
        .output(String::new())
        .status(ToolCallStatus::Pending)
        .build()
}

/// Internal transcript role selector used while coalescing streamed chunks.
#[derive(Debug, Clone, Copy)]
enum TranscriptRole {
    /// Coalesce as assistant text.
    Assistant,
    /// Coalesce as reasoning text.
    Reasoning,
}

/// Extracts visible text from an ACP content block.
fn content_block_text(content: &ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::Image(_) => Some("[image]".to_string()),
        ContentBlock::Audio(_) => Some("[audio]".to_string()),
        ContentBlock::ResourceLink(resource) => Some(format!("[resource] {}", resource.uri)),
        ContentBlock::Resource(_) => Some("[resource]".to_string()),
        _ => None,
    }
}

/// Extracts visible text from ACP tool-call content.
fn tool_content_text(content: &ToolCallContent) -> Option<String> {
    match content {
        ToolCallContent::Content(content) => content_block_text(&content.content),
        ToolCallContent::Diff(_) => Some("[diff]".to_string()),
        ToolCallContent::Terminal(_) => Some("[terminal]".to_string()),
        _ => None,
    }
}

/// Converts an ACP tool call id into the stable string key used by the UI map.
fn tool_call_id_string(tool_call_id: &ToolCallId) -> String {
    tool_call_id.0.to_string()
}

/// Converts JSON values into compact display text.
fn json_to_display(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ContentBlock, ContentChunk, RequestPermissionRequest, TextContent, ToolCall,
        ToolCallId, ToolCallUpdate, ToolCallUpdateFields, UsageUpdate,
    };
    use unicode_width::UnicodeWidthStr;

    /// Builds an ACP session id for state tests.
    fn sid(value: &str) -> SessionId {
        SessionId::new(value.to_string())
    }

    /// Builds a text content block for ACP test fixtures.
    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text(TextContent::new(value))
    }

    /// Builds a session notification for ACP state tests.
    fn notification(session_id: SessionId, update: SessionUpdate) -> SessionNotification {
        SessionNotification::new(session_id, update)
    }

    /// Returns the tool call cell at the given transcript index.
    fn transcript_tool(state: &AppState, index: usize) -> &ToolCallView {
        match &state.transcript()[index] {
            TranscriptCell::ToolCall(tool) => tool,
            other => panic!("expected tool cell, got {other:?}"),
        }
    }

    /// Verifies ACP assistant message chunks are coalesced into one transcript cell.
    #[test]
    fn state_acp_message_chunks_append_to_assistant_cell() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "deepseek/model".to_string(),
        );

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hel"))),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("lo"))),
        ));

        assert_eq!(state.transcript()[0].text(), "hello");
    }

    /// Verifies ACP reasoning chunks are coalesced independently from assistant text.
    #[test]
    fn state_acp_thought_chunks_append_to_reasoning_cell() {
        let session_id = sid("s1");
        let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::AgentThoughtChunk(ContentChunk::new(text("think"))),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::AgentThoughtChunk(ContentChunk::new(text("ing"))),
        ));

        assert_eq!(
            state.transcript()[0],
            TranscriptCell::Reasoning("thinking".to_string())
        );
    }

    /// Verifies ACP tool-call snapshots and updates become renderable tool state.
    #[test]
    fn state_acp_tool_call_and_update_tool_state() {
        let session_id = sid("s1");
        let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new("call-1"), "shell")
                    .status(ToolCallStatus::InProgress)
                    .raw_input(serde_json::json!({"cmd": "pwd"})),
            ),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .content(vec![ToolCallContent::Content(Content::new(text("/tmp")))])
                    .status(ToolCallStatus::Completed),
            )),
        ));

        let tool = transcript_tool(&state, 0);
        assert_eq!(tool.name(), "shell");
        assert_eq!(tool.arguments(), "{\"cmd\":\"pwd\"}");
        assert_eq!(tool.output(), "/tmp");
        assert_eq!(tool.status(), ToolCallStatus::Completed);
    }

    /// Verifies tool calls keep their original transcript position when updated.
    #[test]
    fn state_tool_call_update_mutates_existing_transcript_cell() {
        let session_id = sid("s1");
        let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("before"))),
        ));
        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new("call-1"), "shell")
                    .status(ToolCallStatus::InProgress)
                    .raw_input(serde_json::json!({"command": "pwd"})),
            ),
        ));
        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("after"))),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .content(vec![ToolCallContent::Content(Content::new(text("done")))])
                    .status(ToolCallStatus::Completed),
            )),
        ));

        assert_eq!(state.transcript().len(), 3);
        assert_eq!(state.transcript()[0].text(), "before");
        let tool = transcript_tool(&state, 1);
        assert_eq!(tool.output(), "done");
        assert_eq!(tool.status(), ToolCallStatus::Completed);
        assert_eq!(state.transcript()[2].text(), "after");
    }

    /// Verifies updates arriving before snapshots create a pending transcript tool cell.
    #[test]
    fn state_tool_call_update_first_creates_pending_transcript_cell() {
        let session_id = sid("s1");
        let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .title("shell")
                    .content(vec![ToolCallContent::Content(Content::new(text(
                        "partial",
                    )))]),
            )),
        ));

        let tool = transcript_tool(&state, 0);
        assert_eq!(tool.call_id(), "call-1");
        assert_eq!(tool.name(), "shell");
        assert_eq!(tool.output(), "partial");
        assert_eq!(tool.status(), ToolCallStatus::Pending);
    }

    /// Verifies ACP updates from another session do not mutate renderable state.
    #[test]
    fn state_ignores_acp_updates_for_other_sessions() {
        let mut state = AppState::new(sid("s1"), "/tmp/project".into(), "model".to_string());

        state.apply_session_update(notification(
            sid("s2"),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hello"))),
        ));

        assert!(state.transcript().is_empty());
    }

    /// Verifies ACP usage updates set the status token total.
    #[test]
    fn state_acp_usage_update_sets_total_tokens() {
        let session_id = sid("s1");
        let mut state = AppState::new(session_id.clone(), "/tmp".into(), "model".to_string());

        state.apply_session_update(notification(
            session_id,
            SessionUpdate::UsageUpdate(UsageUpdate::new(30, 0)),
        ));

        assert_eq!(state.usage().total_tokens(), 30);
    }

    /// Verifies the bottom status line includes runtime identity and total usage.
    #[test]
    fn state_bottom_status_includes_model_cwd_and_tokens() {
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "deepseek/model".to_string(),
        );
        state.usage = UsageView::new(10, 20);

        let status = state.bottom_status_line(80);

        assert!(status.contains("deepseek/model"));
        assert!(status.contains("/tmp/project"));
        assert!(status.contains("30"));
    }

    /// Verifies the bottom status line never exceeds narrow terminal display width.
    #[test]
    fn state_bottom_status_line_fits_narrow_width() {
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/very-long-project-directory-name".into(),
            "very-long-provider/very-long-model-name".to_string(),
        );
        state.usage = UsageView::new(12345, 67890);

        let status = state.bottom_status_line(20);

        assert!(UnicodeWidthStr::width(status.as_str()) <= 20);
    }

    /// Verifies token usage remains visible when model and cwd must be truncated.
    #[test]
    fn state_bottom_status_line_prioritizes_tokens_at_narrow_width() {
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/very-long-project-directory-name".into(),
            "very-long-provider/very-long-model-name".to_string(),
        );
        state.usage = UsageView::new(12, 34);

        let status = state.bottom_status_line(36);

        assert!(status.contains("tokens: 46"));
        assert!(UnicodeWidthStr::width(status.as_str()) <= 36);
    }

    /// Verifies status truncation obeys very small width constraints.
    #[test]
    fn state_bottom_status_line_handles_very_small_widths() {
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/very-long-project-directory-name".into(),
            "very-long-provider/very-long-model-name".to_string(),
        );
        state.usage = UsageView::new(12345, 67890);

        for width in [0usize, 1, 2, 3] {
            let status = state.bottom_status_line(width);
            assert!(UnicodeWidthStr::width(status.as_str()) <= width);
        }
    }

    /// Verifies completed prompts clear the running marker and remember the stop reason.
    #[test]
    fn state_finish_prompt_clears_running_status_and_records_stop_reason() {
        let mut state = AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
        state.append_user_message("hello");

        state.finish_prompt(StopReason::EndTurn);

        assert!(!state.is_running_prompt());
        assert_eq!(state.last_stop_reason(), Some(StopReason::EndTurn));
    }

    /// Verifies ACP permission overlays are stored until the UI consumes them.
    #[test]
    fn state_pending_approval_can_be_set_and_taken() {
        let mut state = AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
        let request = RequestPermissionRequest::new(
            sid("s1"),
            ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().title("shell"),
            ),
            vec![],
        );

        state.set_pending_approval(PendingApproval::from_request(11, &request));

        assert_eq!(state.pending_approval().expect("approval").request_id(), 11);
        assert_eq!(
            state
                .take_pending_approval()
                .expect("approval")
                .request_id(),
            11
        );
        assert!(state.pending_approval().is_none());
    }

    /// Verifies runtime errors clear stale approval prompts and become the top status.
    #[test]
    fn state_error_clears_pending_approval() {
        let mut state = AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
        let request = RequestPermissionRequest::new(
            sid("s1"),
            ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().title("shell"),
            ),
            vec![],
        );
        state.set_pending_approval(PendingApproval::from_request(11, &request));

        state.set_error("runtime failed");

        assert!(state.pending_approval().is_none());
        assert_eq!(state.top_status_line(), "error: runtime failed");
    }

    /// Verifies a new user prompt clears stale approval prompts and starts running.
    #[test]
    fn state_append_user_message_clears_pending_approval() {
        let mut state = AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
        let request = RequestPermissionRequest::new(
            sid("s1"),
            ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().title("shell"),
            ),
            vec![],
        );
        state.set_pending_approval(PendingApproval::from_request(11, &request));

        state.append_user_message("next prompt");

        assert!(state.pending_approval().is_none());
        assert!(state.is_running_prompt());
    }
}
