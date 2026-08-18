use super::super::*;

/// Correlated terminal writer for one compaction whose start event was emitted.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct CompactionLifecycle<'a> {
    kernel: &'a Kernel,
    session: &'a Session,
    emitter: &'a EventEmitter,
    run_id: &'a RunId,
    turn_id: &'a TurnId,
    reason: CompactionReason,
}

impl CompactionLifecycle<'_> {
    /// Durably records the successful terminal state before publishing it.
    pub(super) async fn complete(
        &self,
        result: &CompactionResult,
    ) -> Result<(), KernelError> {
        serde_json::to_value(protocol::CompactionOperationFinished {
            outcome: protocol::CompactionOperationOutcome::Completed,
            error: None,
        })
        .map_err(KernelError::from)
        .and_then(|payload| {
            self.kernel.record_operation(
                self.session,
                self.run_id,
                RecordKind::OperationFinished,
                payload,
            )
        })?;
        self.session.sync_store()?;
        self.emitter
            .emit_at(
                self.turn_id.clone(),
                result.ended_at_ms,
                AgentEventPayload::CompactionEnd {
                    run_id: self.run_id.clone(),
                    reason: self.reason,
                    outcome: protocol::CompactionOutcome::Completed {
                        result: result.clone(),
                    },
                },
            )
            .await
    }

    /// Durably records the failed or cancelled terminal state before publishing it.
    pub(super) async fn fail(
        &self,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        let cancelled =
            matches!(error, KernelError::Model(ModelError::Cancelled));
        let outcome = if cancelled {
            protocol::CompactionOutcome::Cancelled
        } else {
            protocol::CompactionOutcome::Failed {
                message: "context compaction failed".to_string(),
            }
        };
        serde_json::to_value(protocol::CompactionOperationFinished {
            outcome: if cancelled {
                protocol::CompactionOperationOutcome::Aborted
            } else {
                protocol::CompactionOperationOutcome::Failed
            },
            error: Some(protocol::CompactionOperationError {
                code: "compaction_failed".to_string(),
                message: error.to_string(),
            }),
        })
        .map_err(KernelError::from)
        .and_then(|payload| {
            self.kernel.record_operation(
                self.session,
                self.run_id,
                RecordKind::OperationFinished,
                payload,
            )
        })?;
        self.session.sync_store()?;
        self.emitter
            .emit_at(
                self.turn_id.clone(),
                self.kernel.clock.now(),
                AgentEventPayload::CompactionEnd {
                    run_id: self.run_id.clone(),
                    reason: self.reason,
                    outcome,
                },
            )
            .await
    }
}
