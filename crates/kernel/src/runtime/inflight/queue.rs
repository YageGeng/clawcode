use std::collections::VecDeque;

use crate::tools::ToolCallRequest;

/// Queue of tool calls whose output items have fully completed during the current stream.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompletedToolCallQueue {
    calls: VecDeque<ToolCallRequest>,
}

impl CompletedToolCallQueue {
    /// Registers one completed tool call in the same order the model finished it.
    pub(crate) fn push_completed(&mut self, call: ToolCallRequest) {
        self.calls.push_back(call);
    }

    /// Returns true when the stream has not yet completed any executable tool call items.
    pub(crate) fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Consumes the queue and returns the completed tool calls in their preserved order.
    pub(crate) fn into_calls(self) -> Vec<ToolCallRequest> {
        self.calls.into_iter().collect()
    }
}
