use agent_client_protocol::schema as official_acp;
use tools::ToolApprovalRequest;

const ALLOW_ONCE_OPTION_ID: &str = "allow_once";
const ALLOW_ALWAYS_OPTION_ID: &str = "allow_always";
const REJECT_ONCE_OPTION_ID: &str = "reject_once";

/// Builds the ACP permission request for one high-risk tool invocation.
pub(crate) fn build_tool_permission_request(
    request: &ToolApprovalRequest,
) -> official_acp::RequestPermissionRequest {
    let tool_call_id = request
        .call_id
        .clone()
        .unwrap_or_else(|| format!("approval-{}", request.tool));

    let mut fields = official_acp::ToolCallUpdateFields::default();
    fields.title = Some(format!("Approve tool `{}`", request.tool));
    fields.raw_input = Some(request.arguments.clone());
    let tool_call = official_acp::ToolCallUpdate::new(tool_call_id, fields);

    official_acp::RequestPermissionRequest::new(
        request.session_id.clone(),
        tool_call,
        vec![
            official_acp::PermissionOption::new(
                ALLOW_ONCE_OPTION_ID,
                "Allow once",
                official_acp::PermissionOptionKind::AllowOnce,
            ),
            official_acp::PermissionOption::new(
                REJECT_ONCE_OPTION_ID,
                "Reject once",
                official_acp::PermissionOptionKind::RejectOnce,
            ),
        ],
    )
}

/// Returns whether an ACP permission response selected an allow option.
pub(crate) fn permission_response_approved(
    response: &official_acp::RequestPermissionResponse,
) -> bool {
    match &response.outcome {
        official_acp::RequestPermissionOutcome::Selected(selected) => {
            matches!(
                selected.option_id.0.as_ref(),
                ALLOW_ONCE_OPTION_ID | ALLOW_ALWAYS_OPTION_ID
            )
        }
        official_acp::RequestPermissionOutcome::Cancelled => false,
        _ => false,
    }
}
