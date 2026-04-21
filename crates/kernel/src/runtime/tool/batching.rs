use crate::tools::{
    executor::{ToolExecutionMode, ToolExecutionQueue, ToolExecutionRequest},
    router::ToolRouter,
};

/// One execution batch derived from a plan after applying per-tool parallel-safety rules.
#[derive(Debug, Clone)]
pub(super) struct ToolExecutionBatch {
    pub(super) mode: ToolExecutionMode,
    pub(super) queue: ToolExecutionQueue,
}

/// Splits one tool execution plan into batches according to the selected mode and tool capabilities.
pub(super) fn build_tool_execution_batches(
    router: &ToolRouter,
    mode: ToolExecutionMode,
    calls: Vec<ToolExecutionRequest>,
) -> Vec<ToolExecutionBatch> {
    if mode == ToolExecutionMode::Serial {
        return vec![ToolExecutionBatch {
            mode: ToolExecutionMode::Serial,
            queue: ToolExecutionQueue::from_requests(calls),
        }];
    }

    let mut batches = Vec::new();
    let mut parallel_batch = Vec::new();

    for call in calls {
        if router.tool_supports_parallel(&call.call.name) {
            parallel_batch.push(call);
            continue;
        }

        if !parallel_batch.is_empty() {
            batches.push(ToolExecutionBatch {
                mode: ToolExecutionMode::Parallel,
                queue: ToolExecutionQueue::from_requests(std::mem::take(&mut parallel_batch)),
            });
        }

        batches.push(ToolExecutionBatch {
            mode: ToolExecutionMode::Serial,
            queue: ToolExecutionQueue::from_requests(vec![call]),
        });
    }

    if !parallel_batch.is_empty() {
        batches.push(ToolExecutionBatch {
            mode: ToolExecutionMode::Parallel,
            queue: ToolExecutionQueue::from_requests(parallel_batch),
        });
    }

    batches
}
