//! Renderable TUI state reduced from ACP session updates.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::{
    ContentBlock, SessionId, SessionNotification, SessionUpdate, StopReason,
    ToolCall, ToolCallContent, ToolCallId, ToolCallStatus, ToolCallUpdate,
    UsageUpdate,
};
use protocol::AgentStatus;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::approval::PendingApproval;
use crate::ui::cell::{TextCell, TextRole, ToolCallCell, TranscriptCell};
use crate::ui::theme::Theme;
use crate::ui::transcript::entry::{
    TranscriptEntry, TranscriptEntryId, TranscriptEntryState,
};

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

    /// Builds a usage view from ACP metadata when Clawcode provides split totals.
    pub fn from_acp_update(update: &UsageUpdate) -> Self {
        if let Some(meta) = &update.meta
            && let Some(usage) =
                meta.get("clawcode").and_then(|value| value.get("usage"))
        {
            let input_tokens = usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let output_tokens = usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let total_tokens = usage
                .get("total_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(input_tokens + output_tokens);
            return Self {
                input_tokens,
                output_tokens,
                total_tokens,
            };
        }
        Self::from_total(update.used)
    }

    /// Returns the compact status text for token usage.
    pub fn status_text(&self) -> String {
        if self.input_tokens == 0 && self.output_tokens == 0 {
            return format!("tokens: {}", self.total_tokens);
        }
        format!(
            "tokens: {} (in {} / out {})",
            self.total_tokens, self.input_tokens, self.output_tokens
        )
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
    /// Color theme used by TUI renderers.
    #[builder(default)]
    theme: Theme,
    /// Ordered transcript entries ready for rendering.
    #[builder(default)]
    transcript: Vec<TranscriptEntry>,
    /// Next stable transcript entry id.
    #[builder(default)]
    next_transcript_entry_id: u64,
    /// Transcript entry index for each tool call id.
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
    /// Latest externally reported lifecycle status for an agent-backed session.
    #[builder(default, setter(strip_option))]
    agent_status: Option<AgentStatus>,
}

impl AppState {
    /// Creates renderable state for a new TUI ACP session.
    pub fn new(
        session_id: SessionId,
        cwd: PathBuf,
        model_label: String,
    ) -> Self {
        AppState::builder()
            .session_id(session_id)
            .cwd(cwd)
            .model_label(model_label)
            .build()
    }

    /// Creates renderable state with an explicit TUI theme.
    pub fn new_with_theme(
        session_id: SessionId,
        cwd: PathBuf,
        model_label: String,
        theme: Theme,
    ) -> Self {
        AppState::builder()
            .session_id(session_id)
            .cwd(cwd)
            .model_label(model_label)
            .theme(theme)
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

    /// Returns the configured TUI render theme.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Returns the renderable transcript entries.
    pub fn transcript(&self) -> &[TranscriptEntry] {
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
                    self.push_committed_text(TextRole::User, text);
                }
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    self.append_to_active_text_or_push(
                        TextRole::Assistant,
                        text,
                    );
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    self.append_to_active_text_or_push(
                        TextRole::Reasoning,
                        text,
                    );
                }
            }
            SessionUpdate::ToolCall(tool_call) => {
                self.apply_tool_call(tool_call)
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.apply_tool_call_update(update)
            }
            SessionUpdate::Plan(_) => {}
            SessionUpdate::UsageUpdate(update) => {
                self.apply_usage_update(update)
            }
            _ => {}
        }
    }

    /// Appends a user prompt and marks the runtime as waiting for a turn result.
    pub fn append_user_message(&mut self, text: impl Into<String>) {
        self.push_committed_text(TextRole::User, text);
        self.running_prompt = true;
        self.last_error = None;
        self.last_stop_reason = None;
        self.pending_approval = None;
        self.agent_status = None;
    }

    /// Records a prompt completion returned by ACP.
    pub fn finish_prompt(&mut self, stop_reason: StopReason) {
        for entry in &mut self.transcript {
            if entry.state() == TranscriptEntryState::Active
                && entry.text_cell().is_some()
            {
                entry.commit();
            }
        }
        self.running_prompt = false;
        self.last_stop_reason = Some(stop_reason);
        self.pending_approval = None;
        self.agent_status = None;
    }

    /// Records externally reported lifecycle status for this agent session.
    pub fn apply_agent_status(&mut self, status: AgentStatus) {
        self.agent_status = Some(status);
    }

    /// Records an ACP permission request in renderable form.
    pub fn set_pending_approval(&mut self, approval: PendingApproval) {
        self.pending_approval = Some(approval);
    }

    /// Records an error message in both state and the visible transcript.
    pub fn set_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.last_error = Some(message.clone());
        self.push_committed_text(TextRole::System, message);
        self.running_prompt = false;
        self.pending_approval = None;
    }

    /// Adds a system message without changing prompt or error state.
    pub fn add_system_message(&mut self, message: impl Into<String>) {
        self.push_committed_text(TextRole::System, message);
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
        if let Some(status_line) = self.agent_status_line() {
            return status_line;
        }
        if let Some(stop_reason) = self.last_stop_reason {
            return format!("stopped: {stop_reason:?}");
        }
        "idle".to_string()
    }

    /// Builds the bottom status line with model, cwd, and token usage.
    pub fn bottom_status_line(&self, width: usize) -> String {
        let token_status = self.usage.status_text();
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

    /// Returns a status-line label for externally reported lifecycle status.
    fn agent_status_line(&self) -> Option<String> {
        let status = self.agent_status.as_ref()?;
        match status {
            AgentStatus::PendingInit => Some("pending".to_string()),
            AgentStatus::Running => Some("running".to_string()),
            AgentStatus::Interrupted => Some("interrupted".to_string()),
            AgentStatus::Completed { .. } => Some("completed".to_string()),
            AgentStatus::Errored { reason } => Some(format!("error: {reason}")),
            AgentStatus::Shutdown => Some("shutdown".to_string()),
            AgentStatus::NotFound => Some("not found".to_string()),
        }
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
        let content = tool_call.content;

        self.mutate_tool_call(call_id, |entry| {
            entry.set_name(title);
            entry.set_arguments(raw_input);
            entry.set_status(status);
            for content in content {
                apply_tool_call_content(entry, content);
            }
        });
    }

    /// Applies an ACP tool-call field update to an existing or placeholder view.
    fn apply_tool_call_update(&mut self, update: ToolCallUpdate) {
        let call_id = tool_call_id_string(&update.tool_call_id);
        self.mutate_tool_call(call_id, |entry| {
            if let Some(title) = update.fields.title {
                entry.set_name(title);
            }
            if let Some(raw_input) = update.fields.raw_input {
                entry.set_arguments(json_to_display(&raw_input));
            }
            if let Some(raw_output) = update.fields.raw_output {
                entry.push_output(&json_to_display(&raw_output));
            }
            if let Some(content) = update.fields.content {
                if content
                    .iter()
                    .any(|content| matches!(content, ToolCallContent::Diff(_)))
                {
                    // ACP content updates replace the content collection; clear
                    // existing diffs so streaming apply_patch previews do not stack.
                    entry.clear_diffs();
                }
                for content in content {
                    apply_tool_call_content(entry, content);
                }
            }
            if let Some(status) = update.fields.status {
                entry.set_status(status);
            }
        });
    }

    /// Applies an ACP token usage update.
    fn apply_usage_update(&mut self, update: UsageUpdate) {
        if update.meta.is_some() {
            self.usage = UsageView::from_acp_update(&update);
        } else {
            self.usage = UsageView::from_total(update.used);
        }
    }

    /// Allocates the next stable transcript entry id.
    fn next_entry_id(&mut self) -> TranscriptEntryId {
        let id = TranscriptEntryId::new(self.next_transcript_entry_id);
        self.next_transcript_entry_id =
            self.next_transcript_entry_id.wrapping_add(1);
        id
    }

    /// Pushes a new transcript entry and returns its index.
    fn push_entry(
        &mut self,
        state: TranscriptEntryState,
        cell: Arc<dyn TranscriptCell>,
    ) -> usize {
        let id = self.next_entry_id();
        let index = self.transcript.len();
        self.transcript.push(TranscriptEntry::new(id, state, cell));
        index
    }

    /// Pushes a committed text entry.
    fn push_committed_text(&mut self, role: TextRole, text: impl Into<String>) {
        self.push_entry(
            TranscriptEntryState::Committed,
            Arc::new(TextCell::new(role, text)),
        );
    }

    /// Returns the trailing active text entry index for the requested role.
    fn trailing_active_text_entry_index(
        &self,
        role: TextRole,
    ) -> Option<usize> {
        let index = self.transcript.len().checked_sub(1)?;
        let entry = self.transcript.get(index)?;
        if entry.state() != TranscriptEntryState::Active {
            return None;
        }
        let text = entry.text_cell()?;
        (text.role() == role).then_some(index)
    }

    /// Appends text to the last active text entry or creates a new active entry.
    fn append_to_active_text_or_push(&mut self, role: TextRole, text: String) {
        if let Some(index) = self.trailing_active_text_entry_index(role)
            && let Some(entry) = self.transcript.get_mut(index)
            && let Some(cell) = entry.text_cell()
        {
            let mut updated = cell.clone();
            updated.push_str(&text);
            entry.replace_cell(Arc::new(updated));
            return;
        }

        self.push_entry(
            TranscriptEntryState::Active,
            Arc::new(TextCell::new(role, text)),
        );
    }

    /// Returns an existing tool entry index or creates a pending placeholder.
    fn pending_tool_call_entry_index(&mut self, call_id: String) -> usize {
        if let Some(index) = self.tool_call_indices.get(&call_id) {
            return *index;
        }

        let index = self.push_entry(
            TranscriptEntryState::Active,
            Arc::new(ToolCallCell::pending(call_id.clone())),
        );
        self.tool_call_indices.insert(call_id, index);
        index
    }

    /// Applies a mutation to a copied tool-call entry and bumps only that entry.
    fn mutate_tool_call(
        &mut self,
        call_id: String,
        mutate: impl FnOnce(&mut ToolCallCell),
    ) {
        let index = self.pending_tool_call_entry_index(call_id);
        let Some(entry) = self.transcript.get_mut(index) else {
            unreachable!("tool call index must point inside transcript");
        };
        let Some(tool) = entry.tool_call() else {
            unreachable!(
                "tool call index must point at a tool call transcript cell"
            );
        };
        let was_committed = entry.state() == TranscriptEntryState::Committed;
        let mut updated = tool.clone();
        mutate(&mut updated);
        if was_committed {
            entry.replace_cell(Arc::new(updated));
            entry.commit();
            return;
        }

        let is_terminal = matches!(
            updated.status(),
            ToolCallStatus::Completed | ToolCallStatus::Failed
        );
        entry.replace_cell(Arc::new(updated));
        if is_terminal {
            entry.commit();
        } else {
            entry.activate();
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

/// Applies one ACP tool-call content item to the renderable tool cell.
fn apply_tool_call_content(entry: &mut ToolCallCell, content: ToolCallContent) {
    match content {
        ToolCallContent::Content(content) => {
            if let Some(text) = content_block_text(&content.content) {
                entry.push_output(&text);
            }
        }
        ToolCallContent::Diff(diff) => {
            entry.push_diff(diff.path, diff.old_text, diff.new_text);
        }
        ToolCallContent::Terminal(_) => {
            entry.push_output("[terminal]");
        }
        _ => {}
    }
}

/// Extracts visible text from an ACP content block.
fn content_block_text(content: &ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::Image(_) => Some("[image]".to_string()),
        ContentBlock::Audio(_) => Some("[audio]".to_string()),
        ContentBlock::ResourceLink(resource) => {
            Some(format!("[resource] {}", resource.uri))
        }
        ContentBlock::Resource(_) => Some("[resource]".to_string()),
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
        Content, ContentBlock, ContentChunk, RequestPermissionRequest,
        TextContent, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, UsageUpdate,
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
    fn notification(
        session_id: SessionId,
        update: SessionUpdate,
    ) -> SessionNotification {
        SessionNotification::new(session_id, update)
    }

    /// Returns the tool call cell at the given transcript index.
    fn transcript_tool(state: &AppState, index: usize) -> &ToolCallCell {
        state.transcript()[index]
            .tool_call()
            .expect("expected tool cell")
    }

    /// Returns the text cell at the given transcript index.
    fn transcript_text(state: &AppState, index: usize) -> &TextCell {
        state.transcript()[index]
            .text_cell()
            .expect("expected text cell")
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

        assert_eq!(transcript_text(&state, 0).text(), "hello");
        assert_eq!(state.transcript()[0].state(), TranscriptEntryState::Active);
    }

    /// Verifies ACP reasoning chunks are coalesced independently from assistant text.
    #[test]
    fn state_acp_thought_chunks_append_to_reasoning_cell() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::AgentThoughtChunk(ContentChunk::new(text("think"))),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::AgentThoughtChunk(ContentChunk::new(text("ing"))),
        ));

        assert_eq!(transcript_text(&state, 0).text(), "thinking");
    }

    /// Verifies ACP tool-call snapshots and updates become renderable tool state.
    #[test]
    fn state_acp_tool_call_and_update_tool_state() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

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
                    .content(vec![ToolCallContent::Content(Content::new(
                        text("/tmp"),
                    ))])
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
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

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
                    .content(vec![ToolCallContent::Content(Content::new(
                        text("done"),
                    ))])
                    .status(ToolCallStatus::Completed),
            )),
        ));

        assert_eq!(state.transcript().len(), 3);
        assert_eq!(transcript_text(&state, 0).text(), "before");
        let tool = transcript_tool(&state, 1);
        assert_eq!(tool.output(), "done");
        assert_eq!(tool.status(), ToolCallStatus::Completed);
        assert_eq!(transcript_text(&state, 2).text(), "after");
    }

    /// Verifies updates arriving before snapshots create a pending transcript tool cell.
    #[test]
    fn state_tool_call_update_first_creates_pending_transcript_cell() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().title("shell").content(vec![
                    ToolCallContent::Content(Content::new(text("partial"))),
                ]),
            )),
        ));

        let tool = transcript_tool(&state, 0);
        assert_eq!(tool.call_id(), "call-1");
        assert_eq!(tool.name(), "shell");
        assert_eq!(tool.output(), "partial");
        assert_eq!(tool.status(), ToolCallStatus::Pending);
    }

    /// Verifies ACP diff content reaches the renderable tool cell as real diff lines.
    #[test]
    fn state_tool_call_update_preserves_diff_content() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .title("apply_patch")
                    .content(vec![ToolCallContent::Diff(
                        agent_client_protocol::schema::Diff::new(
                            "src/main.rs",
                            "fn new() {}\n",
                        )
                        .old_text("fn old() {}\n"),
                    )])
                    .status(ToolCallStatus::Completed),
            )),
        ));

        let tool = transcript_tool(&state, 0);
        let rendered = tool
            .display_lines(80)
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains("--- src/main.rs")));
        assert!(rendered.iter().any(|line| line.contains("+++ src/main.rs")));
        assert!(rendered.iter().any(|line| line.contains("@@ -1,1 +1,1 @@")));
        assert!(rendered.iter().any(|line| line.contains("-fn old() {}")));
        assert!(rendered.iter().any(|line| line.contains("+fn new() {}")));
        assert!(!rendered.iter().any(|line| line.contains("[diff]")));
    }

    /// Verifies ACP diff updates replace previous diff content instead of accumulating previews.
    #[test]
    fn state_tool_call_diff_updates_replace_previous_diff_content() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .title("apply_patch")
                    .content(vec![ToolCallContent::Diff(
                        agent_client_protocol::schema::Diff::new(
                            "src/main.rs",
                            "fn v1() {}\n",
                        )
                        .old_text("fn old() {}\n"),
                    )]),
            )),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().content(vec![
                    ToolCallContent::Diff(
                        agent_client_protocol::schema::Diff::new(
                            "src/main.rs",
                            "fn v2() {}\n",
                        )
                        .old_text("fn old() {}\n"),
                    ),
                ]),
            )),
        ));

        let tool = transcript_tool(&state, 0);
        let rendered = tool
            .raw_lines()
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();

        assert!(!rendered.iter().any(|line| line.contains("+fn v1() {}")));
        assert!(rendered.iter().any(|line| line.contains("+fn v2() {}")));
    }

    /// Verifies completed prompts mark streaming text entries as committed.
    #[test]
    fn state_finish_prompt_commits_active_text_entries() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id,
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hello"))),
        ));
        state.finish_prompt(StopReason::EndTurn);

        assert_eq!(
            state.transcript()[0].state(),
            TranscriptEntryState::Committed
        );
    }

    /// Verifies late tool updates cannot reactivate a committed tool entry.
    #[test]
    fn state_late_tool_update_keeps_committed_tool_entry_committed() {
        let session_id = sid("s1");
        let call_id = "call-1".to_string();
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new(call_id.as_str()), "shell")
                    .status(ToolCallStatus::Completed),
            ),
        ));
        let first_revision = state.transcript()[0].revision();

        state.apply_session_update(notification(
            session_id,
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new(call_id.as_str()),
                ToolCallUpdateFields::new()
                    .content(vec![ToolCallContent::Content(Content::new(
                        text("late output"),
                    ))])
                    .status(ToolCallStatus::InProgress),
            )),
        ));

        assert!(state.transcript()[0].revision() > first_revision);
        assert_eq!(
            state.transcript()[0].state(),
            TranscriptEntryState::Committed
        );
    }

    /// Verifies ACP updates from another session do not mutate renderable state.
    #[test]
    fn state_ignores_acp_updates_for_other_sessions() {
        let mut state = AppState::new(
            sid("s1"),
            "/tmp/project".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            sid("s2"),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hello"))),
        ));

        assert!(state.transcript().is_empty());
    }

    /// Verifies ACP usage updates accumulate and preserve input/output breakdowns.
    #[test]
    fn state_acp_usage_update_accumulates_input_and_output_tokens() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        let usage_update = UsageUpdate::new(15, 0).meta(
            serde_json::json!({
                "clawcode": {
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 3,
                        "total_tokens": 15
                    }
                }
            })
            .as_object()
            .cloned()
            .expect("usage metadata object"),
        );
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::UsageUpdate(usage_update),
        ));

        assert_eq!(state.usage().input_tokens(), 12);
        assert_eq!(state.usage().output_tokens(), 3);
        assert_eq!(state.usage().total_tokens(), 15);
    }

    /// Verifies ACP usage updates without metadata are treated as cumulative totals.
    #[test]
    fn state_acp_usage_update_without_metadata_sets_total_tokens() {
        let session_id = sid("s1");
        let mut state = AppState::new(
            session_id.clone(),
            "/tmp".into(),
            "model".to_string(),
        );

        state.apply_session_update(notification(
            session_id.clone(),
            SessionUpdate::UsageUpdate(UsageUpdate::new(10, 0)),
        ));
        state.apply_session_update(notification(
            session_id,
            SessionUpdate::UsageUpdate(UsageUpdate::new(20, 0)),
        ));

        assert_eq!(state.usage().total_tokens(), 20);
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
        assert!(status.contains("tokens: 30"));
        assert!(status.contains("in 10"));
        assert!(status.contains("out 20"));
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
        let mut state =
            AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
        state.append_user_message("hello");

        state.finish_prompt(StopReason::EndTurn);

        assert!(!state.is_running_prompt());
        assert_eq!(state.last_stop_reason(), Some(StopReason::EndTurn));
    }

    /// Verifies ACP permission overlays are stored until the UI consumes them.
    #[test]
    fn state_pending_approval_can_be_set_and_taken() {
        let mut state =
            AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
        let request = RequestPermissionRequest::new(
            sid("s1"),
            ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().title("shell"),
            ),
            vec![],
        );

        state.set_pending_approval(PendingApproval::from_request(11, &request));

        assert_eq!(
            state.pending_approval().expect("approval").request_id(),
            11
        );
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
        let mut state =
            AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
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
        let mut state =
            AppState::new(sid("s1"), "/tmp".into(), "model".to_string());
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

    /// Verifies agent lifecycle status does not block local prompt submission.
    #[test]
    fn state_agent_running_status_does_not_mark_prompt_running() {
        let mut state =
            AppState::new(sid("s1"), "/tmp".into(), "model".to_string());

        state.apply_agent_status(AgentStatus::Running);

        assert_eq!(state.top_status_line(), "running");
        assert!(!state.is_running_prompt());
    }
}
