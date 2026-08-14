use std::sync::Arc;

use protocol::{AgentEventPayload, AgentOutcome, RunId, SessionId, TurnId};

use super::{
    CompactionExecution, CompactionReason, EventEmitter, ExtensionEvent,
    Kernel, KernelError, RecordKind, RunResult, SessionRuntime,
};

impl RunSettlement<'_> {
    /// Returns the durable pi operation payload for this terminal state.
    fn record_payload(&self) -> serde_json::Value {
        match self.outcome {
            AgentOutcome::Succeeded => {
                serde_json::json!({ "status": "completed" })
            }
            AgentOutcome::Cancelled => {
                serde_json::json!({ "status": "cancelled" })
            }
            AgentOutcome::Failed { message } => serde_json::json!({
                "status": "failed",
                "error": message,
            }),
        }
    }
}

/// Result produced by the execution loop before terminal lifecycle dispatch.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct RunCompletion {
    /// Complete public result returned after settlement.
    pub(super) result: RunResult,
    /// Last Turn used to correlate terminal events.
    pub(super) turn_id: TurnId,
    /// Typed terminal state persisted and emitted by settlement.
    pub(super) outcome: AgentOutcome,
    /// Optional post-agent compaction performed before `AgentSettled`.
    #[builder(default)]
    pub(super) auto_compaction_reason: Option<CompactionReason>,
}

/// Dependencies required to close one run lifecycle exactly once.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct RunSettlement<'a> {
    kernel: &'a Kernel,
    session_id: &'a SessionId,
    session: &'a Arc<SessionRuntime>,
    emitter: &'a EventEmitter,
    run_id: &'a RunId,
    turn_id: TurnId,
    cancellation: &'a tokio_util::sync::CancellationToken,
    outcome: &'a AgentOutcome,
    #[builder(default)]
    auto_compaction_reason: Option<CompactionReason>,
}

impl RunSettlement<'_> {
    /// Persists and publishes the complete terminal lifecycle in protocol order.
    pub(super) async fn settle(self) -> Result<(), KernelError> {
        self.kernel.record_operation(
            self.session,
            self.run_id,
            RecordKind::OperationFinished,
            self.record_payload(),
        )?;
        self.emitter
            .emit(
                self.turn_id.clone(),
                AgentEventPayload::RunEnd {
                    run_id: self.run_id.clone(),
                    outcome: self.outcome.clone(),
                },
            )
            .await?;
        self.kernel
            .dispatch(
                ExtensionEvent::AgentEnd,
                self.session_id,
                Some(self.turn_id.clone()),
            )
            .await?;
        if let Some(reason) = self.auto_compaction_reason {
            // Pi compacts after AgentEnd and before the final settled event.
            self.kernel
                .perform_compaction(
                    CompactionExecution::builder()
                        .session_id(self.session_id)
                        .session(self.session)
                        .sink(Arc::clone(&self.emitter.sink))
                        .reason(reason)
                        .cancellation(self.cancellation)
                        .build(),
                )
                .await?;
        }
        self.emitter
            .emit(
                self.turn_id,
                AgentEventPayload::AgentSettled {
                    run_id: self.run_id.clone(),
                    outcome: self.outcome.clone(),
                },
            )
            .await?;
        self.kernel
            .dispatch(ExtensionEvent::AgentSettled, self.session_id, None)
            .await?;
        Ok(())
    }
}
