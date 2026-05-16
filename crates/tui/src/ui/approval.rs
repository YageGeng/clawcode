//! Approval overlay state and key mapping.

use agent_client_protocol::schema::{
    ContentBlock, PermissionOptionId, RequestPermissionRequest, ToolCallContent,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// User decision selected from the approval overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Allow the pending operation once.
    AllowOnce,
    /// Reject the pending operation once.
    RejectOnce,
}

impl ApprovalDecision {
    /// Returns the ACP permission option id used by the local ACP server.
    pub fn option_id(self) -> PermissionOptionId {
        match self {
            ApprovalDecision::AllowOnce => PermissionOptionId::new("allow_once"),
            ApprovalDecision::RejectOnce => PermissionOptionId::new("reject_once"),
        }
    }
}

/// Stores the currently displayed approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    /// Local request id used to look up the one-shot ACP responder.
    request_id: u64,
    /// Short heading rendered by the approval overlay.
    title: String,
    /// Detailed text rendered by the approval overlay.
    body: String,
}

impl PendingApproval {
    /// Builds pending approval state from an ACP permission request.
    pub fn from_request(request_id: u64, request: &RequestPermissionRequest) -> Self {
        let title = request
            .tool_call
            .fields
            .title
            .as_deref()
            .unwrap_or_else(|| request.tool_call.tool_call_id.0.as_ref())
            .to_string();
        let body = request
            .tool_call
            .fields
            .content
            .as_ref()
            .map(|content| {
                content
                    .iter()
                    .filter_map(tool_content_text)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .join("\n");

        Self {
            request_id,
            title: format!("Approve {title}"),
            body,
        }
    }

    /// Returns the local request id that must receive the approval decision.
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Returns the approval overlay title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the approval overlay body text.
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Extracts display text from ACP tool-call content.
fn tool_content_text(content: &ToolCallContent) -> Option<String> {
    match content {
        ToolCallContent::Content(content) => match &content.content {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        },
        ToolCallContent::Diff(_) => Some("[diff]".to_string()),
        ToolCallContent::Terminal(_) => Some("[terminal]".to_string()),
        _ => None,
    }
}

/// Maps approval overlay key input to a one-shot approval decision.
pub fn decision_for_key(key: KeyEvent) -> Option<ApprovalDecision> {
    // Raw terminal streams can include repeat and release events; only the
    // initial press should resolve a one-shot approval decision.
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('a' | 'y'), KeyModifiers::NONE) => Some(ApprovalDecision::AllowOnce),
        (KeyCode::Char('r' | 'n'), KeyModifiers::NONE) => Some(ApprovalDecision::RejectOnce),
        (KeyCode::Esc, _) => Some(ApprovalDecision::RejectOnce),
        _ => None,
    }
}

/// Builds overlay content for pending user approval prompts.
pub(crate) fn approval_lines(title: &str, body: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(body.to_string()),
        Line::from(""),
        Line::from(Span::styled(
            "[a] allow once   [r] reject",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ContentBlock, PermissionOption, PermissionOptionKind, RequestPermissionRequest,
        TextContent, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };

    /// Verifies ACP permission requests preserve the local id and renderable details.
    #[test]
    fn approval_from_acp_request_extracts_id_title_and_body() {
        let request = RequestPermissionRequest::new(
            agent_client_protocol::schema::SessionId::new("s1".to_string()),
            ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new()
                    .title("shell")
                    .content(vec![ToolCallContent::Content(Content::new(
                        ContentBlock::Text(TextContent::new("pwd")),
                    ))]),
            ),
            vec![PermissionOption::new(
                "allow_once",
                "Allow Once",
                PermissionOptionKind::AllowOnce,
            )],
        );

        let approval = PendingApproval::from_request(7, &request);

        assert_eq!(approval.request_id(), 7);
        assert_eq!(approval.title(), "Approve shell");
        assert_eq!(approval.body(), "pwd");
    }

    /// Verifies unmodified lower-case approval keys map to approval decisions.
    #[test]
    fn approval_keys_map_to_decisions() {
        let allow = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let allow_yes = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let reject = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let reject_no = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(decision_for_key(allow), Some(ApprovalDecision::AllowOnce));
        assert_eq!(
            decision_for_key(allow_yes),
            Some(ApprovalDecision::AllowOnce)
        );
        assert_eq!(decision_for_key(reject), Some(ApprovalDecision::RejectOnce));
        assert_eq!(
            decision_for_key(reject_no),
            Some(ApprovalDecision::RejectOnce)
        );
        assert_eq!(decision_for_key(escape), Some(ApprovalDecision::RejectOnce));
    }

    /// Verifies modified character keys cannot accidentally resolve an approval decision.
    #[test]
    fn approval_modified_key_combinations_return_none() {
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);

        assert_eq!(decision_for_key(ctrl_a), None);
    }

    /// Verifies held approval keys do not resolve the same approval more than once.
    #[test]
    fn approval_ignores_key_repeat_events() {
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);

        assert_eq!(decision_for_key(repeat), None);
    }

    /// Verifies shifted approval keys are not accepted as lower-case decisions.
    #[test]
    fn approval_shifted_character_keys_return_none() {
        let uppercase_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE);
        let shifted_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT);

        assert_eq!(decision_for_key(uppercase_a), None);
        assert_eq!(decision_for_key(shifted_a), None);
    }

    /// Verifies key release events do not resolve approval decisions.
    #[test]
    fn approval_ignores_key_release_events() {
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );

        assert_eq!(decision_for_key(release), None);
    }
}
