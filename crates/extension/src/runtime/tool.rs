use protocol::{
    ToolBlock, ToolCallEvent, ToolCallResult, ToolExecutionEndEvent,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResultEvent,
    ToolResultPatch, UserBashEvent, UserBashResult,
};

use crate::{
    ExtensionContext, ExtensionRuntime, ToolCallPoint, ToolExecutionEndPoint,
    ToolExecutionStartPoint, ToolExecutionUpdatePoint, ToolResultPoint,
    UserBashPoint,
};

impl ExtensionRuntime {
    /// Chains tool argument replacements and fails closed on handler errors.
    pub async fn emit_tool_call(
        &self,
        mut event: ToolCallEvent,
        context: &ExtensionContext,
    ) -> ToolCallResult {
        let mut replaced = false;
        for registered in self.handlers::<ToolCallPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(ToolCallResult::Continue) => {}
                Ok(ToolCallResult::Replace { arguments }) => {
                    event.call.arguments = arguments;
                    replaced = true;
                }
                Ok(block @ ToolCallResult::Block(_)) => return block,
                Err(error) => {
                    self.report::<ToolCallPoint>(registered, &error).await;
                    return ToolCallResult::Block(
                        ToolBlock::builder()
                            .reason(error.to_string())
                            .terminate(false)
                            .build(),
                    );
                }
            }
        }

        if replaced {
            ToolCallResult::Replace {
                arguments: event.call.arguments,
            }
        } else {
            ToolCallResult::Continue
        }
    }

    /// Chains completed tool-result patches before persistence and end events.
    pub async fn emit_tool_result(
        &self,
        mut event: ToolResultEvent,
        context: &ExtensionContext,
    ) -> ToolResultPatch {
        let mut composed = ToolResultPatch::default();
        for registered in self.handlers::<ToolResultPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(patch) => {
                    if let Some(blocks) = patch.blocks {
                        event.result.blocks = blocks.clone();
                        composed.blocks = Some(blocks);
                    }
                    if let Some(details) = patch.details {
                        event.result.details = Some(details.clone());
                        composed.details = Some(details);
                    }
                    if let Some(is_error) = patch.is_error {
                        event.result.is_error = is_error;
                        composed.is_error = Some(is_error);
                    }
                    if patch.usage.is_some() {
                        composed.usage = patch.usage;
                    }
                }
                Err(error) => {
                    self.report::<ToolResultPoint>(registered, &error).await;
                }
            }
        }
        composed
    }

    /// Returns the first complete replacement for a user-authored shell command.
    pub async fn emit_user_bash(
        &self,
        event: &UserBashEvent,
        context: &ExtensionContext,
    ) -> Option<UserBashResult> {
        for registered in self.handlers::<UserBashPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(Some(result)) => return Some(result),
                Ok(None) => {}
                Err(error) => {
                    self.report::<UserBashPoint>(registered, &error).await;
                }
            }
        }
        None
    }

    /// Notifies extensions before one tool begins execution.
    pub async fn emit_tool_execution_start(
        &self,
        event: &ToolExecutionStartEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<ToolExecutionStartPoint>(event, context)
            .await;
    }

    /// Notifies extensions about one replaceable partial tool result.
    pub async fn emit_tool_execution_update(
        &self,
        event: &ToolExecutionUpdateEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<ToolExecutionUpdatePoint>(event, context)
            .await;
    }

    /// Notifies extensions after one tool result is finalized.
    pub async fn emit_tool_execution_end(
        &self,
        event: &ToolExecutionEndEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<ToolExecutionEndPoint>(event, context).await;
    }
}
