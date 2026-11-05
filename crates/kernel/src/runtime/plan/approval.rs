//! `exit_plan_mode`: submits a finished plan and waits for the user's decision.

use std::sync::{Arc, Weak};

use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    AgentEventPayload, ContentBlock, EXIT_PLAN_MODE_TOOL, IdKind,
    PlanApprovalDecision, PlanApprovalRequest, PlanApprovalResponse, SessionId,
    SessionMode, TimestampMs, ToolCall, ToolDefinition, ToolResult,
    ToolResultDetails, TurnId,
};
use tokio_util::sync::CancellationToken;

use crate::runtime::session::settings::SessionSettingsUpdate;
use crate::runtime::{Kernel, KernelError, Session};

/// Returned to the model when the user approves its plan.
const APPROVED_TEXT: &str = "The user approved the plan. Plan mode has ended; you can now edit files. Carry out the plan.";
/// Returned to the model when the user keeps planning or dismisses the prompt.
const KEEP_PLANNING_TEXT: &str =
    "The user chose to keep planning. Stop and wait for their feedback.";

/// Submits the model's plan for approval; registered in every Session.
pub(in crate::runtime) struct ExitPlanModeTool {
    pub(in crate::runtime) session: Weak<Session>,
    pub(in crate::runtime) kernel: Kernel,
}

impl ExitPlanModeTool {
    /// Builds the model-visible failure for a submission that cannot proceed.
    fn refuse(message: &str) -> ToolError {
        ToolError::Execution {
            tool: EXIT_PLAN_MODE_TOOL.into(),
            message: message.into(),
        }
    }
}

#[async_trait]
impl AgentTool for ExitPlanModeTool {
    /// Declares the tool in every mode so switching keeps the prompt prefix.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: EXIT_PLAN_MODE_TOOL.into(),
            description: "Submit your finished plan for the user's approval. Use this only in plan mode, once your research is done; the user then approves it or asks you to keep planning.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The complete plan as Markdown"
                    }
                },
                "required": ["plan"]
            }),
        }
    }

    /// Requires a plan with visible text.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        let present = call
            .arguments
            .get("plan")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|plan| !plan.trim().is_empty());
        if present {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments {
                tool: EXIT_PLAN_MODE_TOOL.into(),
                message: "plan must be non-empty text".into(),
            })
        }
    }

    /// Shows the plan, waits for the user's decision, and leaves plan mode on approval.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let session = self.session.upgrade().ok_or_else(|| {
            Self::refuse("the Session is no longer available")
        })?;
        let as_tool_error =
            |error: KernelError| Self::refuse(&error.to_string());
        if session.plan.mode().map_err(as_tool_error)? != SessionMode::Plan {
            return Err(Self::refuse("Plan mode is not active."));
        }
        if session.ownership.is_some() {
            return Err(Self::refuse(
                "Subagents cannot leave plan mode; give your findings to the parent session as your final reply.",
            ));
        }
        let plan = call
            .arguments
            .get("plan")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        // The plan appears in the tool card while the user decides.
        context.publish(
            ToolResult::builder()
                .tool_call_id(call.tool_call_id.clone())
                .blocks(vec![ContentBlock::Text { text: plan.clone() }])
                .is_error(false)
                .build(),
        );
        let request = PlanApprovalRequest {
            // Tool call ids can repeat across Turns, so a late answer to an
            // earlier plan must not match this one.
            request_id: self.kernel.id_generator.next(IdKind::Request),
            tool_call_id: call.tool_call_id.clone(),
            plan: plan.clone(),
        };
        let Some(decision) = session
            .await_plan_approval(
                request,
                context.turn_id.clone(),
                &context.cancellation,
            )
            .await
            .map_err(as_tool_error)?
        else {
            return Err(Self::refuse(
                "A plan is already waiting for the user's approval.",
            ));
        };
        if context.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let (text, details) = match decision {
            PlanApprovalDecision::Approve => {
                SessionSettingsUpdate::builder()
                    .kernel(&self.kernel)
                    .session_id(&context.session_id)
                    .session(&session)
                    .turn_id(Some(&context.turn_id))
                    .build()
                    .mode(SessionMode::Default)
                    .await
                    .map_err(as_tool_error)?;
                (
                    APPROVED_TEXT,
                    ToolResultDetails::PlanApproval {
                        approved: true,
                        plan: Some(plan),
                    },
                )
            }
            // The mode is left as it is: the user may already have changed it.
            PlanApprovalDecision::KeepPlanning
            | PlanApprovalDecision::Cancelled => (
                KEEP_PLANNING_TEXT,
                ToolResultDetails::PlanApproval {
                    approved: false,
                    plan: None,
                },
            ),
        };
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text { text: text.into() }])
            .is_error(false)
            .details(details)
            .build())
    }
}

impl Session {
    /// Publishes one plan approval and waits for an answer or the Run's cancellation.
    ///
    /// Returns `None` when another plan is already waiting.
    async fn await_plan_approval(
        &self,
        request: PlanApprovalRequest,
        turn_id: TurnId,
        cancellation: &CancellationToken,
    ) -> Result<Option<PlanApprovalDecision>, KernelError> {
        let request_id = request.request_id.clone();
        let tool_call_id = request.tool_call_id.clone();
        let Some(wait) = self.plan.begin(request.clone())? else {
            return Ok(None);
        };
        tracing::info!(
            "requested plan approval for session {} call {}",
            self.transcript.session_id()?,
            tool_call_id
        );
        // A failed publish drops the wait, which forgets the plan.
        self.execution
            .emit_to_run(
                turn_id.clone(),
                TimestampMs::now(),
                AgentEventPayload::PlanApprovalRequested { request },
            )
            .await?;
        // An MCP elicitation may already have the Run waiting on the user.
        if self.execution.begin_user_wait() {
            self.execution
                .emit_to_run(
                    turn_id.clone(),
                    TimestampMs::now(),
                    AgentEventPayload::ActionRequired {
                        request_id: request_id.clone(),
                    },
                )
                .await?;
        }
        let decision = wait.decision(cancellation).await;
        self.execution
            .emit_to_run(
                turn_id.clone(),
                TimestampMs::now(),
                AgentEventPayload::PlanApprovalResolved {
                    request_id: request_id.clone(),
                    decision,
                },
            )
            .await?;
        if self.execution.end_user_wait() {
            self.execution
                .emit_to_run(
                    turn_id,
                    TimestampMs::now(),
                    AgentEventPayload::ActionResolved { request_id },
                )
                .await?;
        }
        tracing::info!(
            "resolved plan approval for session {} call {} as {:?}",
            self.transcript.session_id()?,
            tool_call_id,
            decision
        );
        Ok(Some(decision))
    }
}

impl Kernel {
    /// Delivers one client's decision to the Session's pending plan approval.
    pub async fn respond_plan_approval(
        &self,
        response: PlanApprovalResponse,
    ) -> Result<(), KernelError> {
        self.session(&response.session_id)?
            .plan
            .resolve(&response.request_id, response.decision)
    }

    /// Returns the plan still waiting for approval, so a resumed client can ask again.
    pub fn plan_approval(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<PlanApprovalRequest>, KernelError> {
        self.session(session_id)?.plan.pending()
    }
}

/// Registers `exit_plan_mode` as the kernel's own tool for one Session.
pub(in crate::runtime) fn register(
    runtime: &Arc<Session>,
    kernel: &Kernel,
) -> Result<(), KernelError> {
    runtime.tools.register_kernel(Arc::new(ExitPlanModeTool {
        session: Arc::downgrade(runtime),
        kernel: kernel.clone(),
    }))?;
    Ok(())
}
