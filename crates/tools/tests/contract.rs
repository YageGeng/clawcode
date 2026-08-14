use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use protocol::{ContentBlock, SessionId, ToolCallId, ToolResult, TurnId};
use tokio_util::sync::CancellationToken;
use tools::{ToolError, ToolExecutionContext, ToolUpdateSink};

/// Captures replaceable tool snapshots without depending on the kernel event sink.
#[derive(Default)]
struct RecordingUpdates(Mutex<Vec<ToolResult>>);

impl ToolUpdateSink for RecordingUpdates {
    /// Stores one partial result in publication order.
    fn publish(&self, update: ToolResult) {
        self.0.lock().expect("updates lock").push(update);
    }
}

/// Builds a fully typed context with explicit cancellation and update ownership.
fn execution_context(
    cancellation: CancellationToken,
    updates: Arc<dyn ToolUpdateSink>,
) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(
            SessionId::try_from("session-contract").expect("session id"),
        )
        .turn_id(TurnId::try_from("turn-contract").expect("turn id"))
        .cwd(PathBuf::from("/workspace"))
        .cancellation(cancellation)
        .updates(updates)
        .build()
}

/// Cancellation is observed through the shared Turn token before side effects.
#[test]
fn execution_context_rejects_work_after_turn_cancellation() {
    let cancellation = CancellationToken::new();
    let context = execution_context(
        cancellation.clone(),
        Arc::new(RecordingUpdates::default()),
    );
    assert_eq!(context.ensure_active(), Ok(()));

    cancellation.cancel();

    assert_eq!(context.ensure_active(), Err(ToolError::Cancelled));
}

/// Tool snapshots retain call correlation when forwarded to the kernel.
#[test]
fn execution_context_publishes_partial_results() {
    let updates = Arc::new(RecordingUpdates::default());
    let context = execution_context(
        CancellationToken::new(),
        Arc::clone(&updates) as Arc<dyn ToolUpdateSink>,
    );
    context.publish(
        ToolResult::builder()
            .tool_call_id(ToolCallId::try_from("call-update").expect("call id"))
            .blocks(vec![ContentBlock::Text {
                text: "partial".to_string(),
            }])
            .is_error(false)
            .build(),
    );

    let captured = updates.0.lock().expect("updates lock");
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].blocks[0].text(), Some("partial"));
}
