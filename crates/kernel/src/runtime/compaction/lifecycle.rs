use super::super::*;

/// Correlated terminal writer for one compaction whose start event was emitted.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct CompactionLifecycle<'a> {
    kernel: &'a Kernel,
    session: &'a SessionRuntime,
    emitter: &'a EventEmitter,
    run_id: &'a RunId,
    turn_id: &'a TurnId,
    reason: CompactionReason,
}

impl CompactionLifecycle<'_> {
    /// Records and emits the successful terminal state while attempting both sinks.
    pub(super) async fn complete(
        &self,
        result: &CompactionResult,
    ) -> Result<(), KernelError> {
        let record_result =
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
            });
        let event_result = self
            .emitter
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
            .await;
        self.combine_terminal_results(record_result, event_result)
    }

    /// Records and emits the failed or cancelled terminal state after an execution error.
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
        let record_result =
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
            });
        let event_result = self
            .emitter
            .emit_at(
                self.turn_id.clone(),
                self.kernel.clock.now(),
                AgentEventPayload::CompactionEnd {
                    run_id: self.run_id.clone(),
                    reason: self.reason,
                    outcome,
                },
            )
            .await;
        self.combine_terminal_results(record_result, event_result)
    }

    /// Preserves the first terminal failure after both durable and event paths were attempted.
    fn combine_terminal_results(
        &self,
        record_result: Result<(), KernelError>,
        event_result: Result<(), KernelError>,
    ) -> Result<(), KernelError> {
        match (record_result, event_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(record_error), Err(event_error)) => {
                tracing::error!(
                    "failed to emit terminal compaction event for Run {} after terminal record failure: {}",
                    self.run_id,
                    event_error
                );
                Err(record_error)
            }
        }
    }
}
