use std::sync::Arc;

use protocol::{ContentBlock, ToolCallId, ToolResult};

use crate::{TerminalOutputSink, ToolUpdateSink};

/// Adapts terminal output snapshots to the existing tool update channel.
pub(super) struct TerminalToolUpdates {
    tool_call_id: ToolCallId,
    updates: Arc<dyn ToolUpdateSink>,
}

impl TerminalToolUpdates {
    /// Binds one terminal interaction to its correlated model tool call.
    pub(super) fn new(
        tool_call_id: ToolCallId,
        updates: Arc<dyn ToolUpdateSink>,
    ) -> Self {
        Self {
            tool_call_id,
            updates,
        }
    }
}

impl TerminalOutputSink for TerminalToolUpdates {
    /// Publishes one replaceable JSON snapshot compatible with the final result.
    fn publish(&self, output: String) {
        self.updates.publish(
            ToolResult::builder()
                .tool_call_id(self.tool_call_id.clone())
                .blocks(vec![ContentBlock::Text {
                    text: serde_json::json!({ "output": output }).to_string(),
                }])
                .is_error(false)
                .build(),
        );
    }
}
