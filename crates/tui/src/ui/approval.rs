//! Approval overlay state and key mapping.

use agent_client_protocol::schema::{
    ContentBlock, PermissionOptionId, RequestPermissionRequest, ToolCallContent,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// User decision selected from the approval overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Approve this request once.
    Approved,
    /// Approve matching requests for this session.
    ApprovedForSession,
    /// Approve and persist a command-prefix amendment.
    ApprovedExecpolicyAmendment(protocol::ExecPolicyAmendment),
    /// Deny this request.
    Denied,
    /// Abort the current turn.
    Abort,
}

impl ApprovalDecision {
    /// Returns the ACP permission option id used by the local ACP server.
    pub fn option_id(&self) -> Option<PermissionOptionId> {
        let option_id = match self {
            ApprovalDecision::Approved => "allow_once",
            ApprovalDecision::ApprovedForSession => "allow_always",
            ApprovalDecision::ApprovedExecpolicyAmendment(_) => {
                "allow_execpolicy_amendment"
            }
            ApprovalDecision::Denied => "reject_once",
            // Abort resolves through the ACP cancellation path, not a selected
            // permission option.
            ApprovalDecision::Abort => return None,
        };
        Some(PermissionOptionId::new(option_id))
    }
}

impl From<ApprovalDecision> for protocol::ReviewDecision {
    /// Convert a TUI approval decision into the enhanced protocol decision.
    fn from(value: ApprovalDecision) -> Self {
        match value {
            ApprovalDecision::Approved => Self::Approved,
            ApprovalDecision::ApprovedForSession => Self::ApprovedForSession,
            ApprovalDecision::ApprovedExecpolicyAmendment(amendment) => {
                Self::ApprovedExecpolicyAmendment {
                    proposed_execpolicy_amendment: amendment,
                }
            }
            ApprovalDecision::Denied => Self::Denied,
            ApprovalDecision::Abort => Self::Abort,
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
    /// Decisions the overlay may present.
    available_decisions: Vec<ApprovalDecision>,
}

impl PendingApproval {
    /// Builds pending approval state from an ACP permission request.
    pub fn from_request(
        request_id: u64,
        request: &RequestPermissionRequest,
    ) -> Self {
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
            available_decisions: Self::decisions_from_options(&request.options),
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

    /// Returns the decisions this overlay may present.
    pub fn available_decisions(&self) -> &[ApprovalDecision] {
        &self.available_decisions
    }

    /// Maps a key input to one of this request's available approval decisions.
    pub fn decision_for_key(&self, key: KeyEvent) -> Option<ApprovalDecision> {
        decision_for_key(key)
            .and_then(|decision| self.match_available(decision))
    }

    /// Convert ACP permission options into local approval decisions.
    fn decisions_from_options(
        options: &[agent_client_protocol::schema::PermissionOption],
    ) -> Vec<ApprovalDecision> {
        let mut decisions = Vec::new();
        for option in options {
            match option.option_id.0.as_ref() {
                "allow_once" => decisions.push(ApprovalDecision::Approved),
                "allow_always" => {
                    decisions.push(ApprovalDecision::ApprovedForSession)
                }
                "allow_execpolicy_amendment" => decisions.push(
                    // The amendment content is not embedded in ACP PermissionOption —
                    // it is stored server-side (ACP agent) and reconstructed from the
                    // original kernel ExecApprovalRequirement when the selected option_id
                    // is received back. The empty Vec here is a kind-marker only; it is
                    // preserved by same_kind() matching and never used directly.
                    ApprovalDecision::ApprovedExecpolicyAmendment(
                        protocol::ExecPolicyAmendment::new(Vec::new()),
                    ),
                ),
                "reject_once" | "reject_always" => {
                    decisions.push(ApprovalDecision::Denied)
                }
                _ => {}
            }
        }

        decisions.push(ApprovalDecision::Abort);
        decisions
    }

    /// Return the matching available decision while preserving stored payloads.
    fn match_available(
        &self,
        requested: ApprovalDecision,
    ) -> Option<ApprovalDecision> {
        self.available_decisions
            .iter()
            .find(|candidate| candidate.same_kind(&requested))
            .cloned()
    }
}

impl ApprovalDecision {
    /// Return true when two decisions represent the same UI action.
    fn same_kind(&self, other: &ApprovalDecision) -> bool {
        matches!(
            (self, other),
            (ApprovalDecision::Approved, ApprovalDecision::Approved)
                | (
                    ApprovalDecision::ApprovedForSession,
                    ApprovalDecision::ApprovedForSession,
                )
                | (
                    ApprovalDecision::ApprovedExecpolicyAmendment(_),
                    ApprovalDecision::ApprovedExecpolicyAmendment(_),
                )
                | (ApprovalDecision::Denied, ApprovalDecision::Denied)
                | (ApprovalDecision::Abort, ApprovalDecision::Abort)
        )
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

/// Maps approval overlay key input to an approval decision.
pub fn decision_for_key(key: KeyEvent) -> Option<ApprovalDecision> {
    // Raw terminal streams can include repeat and release events; only the
    // initial press should resolve a one-shot approval decision.
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('a' | 'y'), KeyModifiers::NONE) => {
            Some(ApprovalDecision::Approved)
        }
        (KeyCode::Char('s'), KeyModifiers::NONE) => {
            Some(ApprovalDecision::ApprovedForSession)
        }
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
            // The amendment content is supplied by the ACP server from the stored
            // kernel ExecApprovalRequirement; the empty Vec here is a kind-marker
            // that same_kind() uses to match against the available decision.
            Some(ApprovalDecision::ApprovedExecpolicyAmendment(
                protocol::ExecPolicyAmendment::new(Vec::new()),
            ))
        }

        (KeyCode::Char('r' | 'n'), KeyModifiers::NONE) => {
            Some(ApprovalDecision::Denied)
        }
        (KeyCode::Esc, _) => Some(ApprovalDecision::Abort),
        _ => None,
    }
}

/// Builds overlay content for pending user approval prompts.
pub(crate) fn approval_lines(
    title: &str,
    body: &str,
    decisions: &[ApprovalDecision],
) -> Vec<Line<'static>> {
    let help = approval_help_text(decisions);
    vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(body.to_string()),
        Line::from(""),
        Line::from(Span::styled(
            help,
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]
}

/// Build compact approval overlay help text from the available decisions.
fn approval_help_text(decisions: &[ApprovalDecision]) -> String {
    let mut parts = Vec::new();
    if decisions
        .iter()
        .any(|decision| matches!(decision, ApprovalDecision::Approved))
    {
        parts.push("[a] approve");
    }
    if decisions.iter().any(|decision| {
        matches!(decision, ApprovalDecision::ApprovedForSession)
    }) {
        parts.push("[s] session");
    }
    if decisions.iter().any(|decision| {
        matches!(decision, ApprovalDecision::ApprovedExecpolicyAmendment(_))
    }) {
        parts.push("[p] policy");
    }
    if decisions
        .iter()
        .any(|decision| matches!(decision, ApprovalDecision::Denied))
    {
        parts.push("[r] reject");
    }
    if decisions
        .iter()
        .any(|decision| matches!(decision, ApprovalDecision::Abort))
    {
        parts.push("[esc] abort");
    }
    parts.join("   ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ContentBlock, PermissionOption, PermissionOptionKind,
        RequestPermissionRequest, TextContent, ToolCallId, ToolCallUpdate,
        ToolCallUpdateFields,
    };

    /// Verifies ACP permission requests preserve the local id and renderable details.
    #[test]
    fn approval_from_acp_request_extracts_id_title_and_body() {
        let request = RequestPermissionRequest::new(
            agent_client_protocol::schema::SessionId::new("s1".to_string()),
            ToolCallUpdate::new(
                ToolCallId::new("call-1"),
                ToolCallUpdateFields::new().title("shell").content(vec![
                    ToolCallContent::Content(Content::new(ContentBlock::Text(
                        TextContent::new("pwd"),
                    ))),
                ]),
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

    /// Verifies approval keys map to enhanced decisions.
    #[test]
    fn approval_keys_map_to_approval_decisions() {
        let allow = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let allow_yes = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let session = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        let reject = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let reject_no = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(decision_for_key(allow), Some(ApprovalDecision::Approved));
        assert_eq!(
            decision_for_key(allow_yes),
            Some(ApprovalDecision::Approved)
        );
        assert_eq!(
            decision_for_key(session),
            Some(ApprovalDecision::ApprovedForSession)
        );
        assert_eq!(decision_for_key(reject), Some(ApprovalDecision::Denied));
        assert_eq!(decision_for_key(reject_no), Some(ApprovalDecision::Denied));
        assert_eq!(decision_for_key(escape), Some(ApprovalDecision::Abort));
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
        let repeat = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        );

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
