use std::sync::Arc;

use async_trait::async_trait;
use extension::ExtensionDiagnosticSink;
use protocol::{
    ContentBlock, ExtensionId, IdGenerator, IdKind, MessageIdentity, TurnId,
};

use super::super::{SessionExecution, SessionTranscript};

/// Failure stage for infrastructure that records extension handler diagnostics.
#[derive(Debug)]
enum DiagnosticStage {
    Identity,
    Persist,
    Sequence,
    Sink,
}

impl std::fmt::Display for DiagnosticStage {
    /// Formats a stable stage value for readable diagnostic messages.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "identity",
            Self::Persist => "persist",
            Self::Sequence => "sequence",
            Self::Sink => "sink",
        })
    }
}

/// Session-owned diagnostic sink that projects sanitized handler failures live.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct KernelExtensionDiagnostics {
    clock: Arc<dyn store::Clock>,
    execution: Arc<SessionExecution>,
    transcript: Arc<SessionTranscript>,
    id_generator: Arc<dyn IdGenerator>,
}

#[async_trait]
impl ExtensionDiagnosticSink for KernelExtensionDiagnostics {
    /// Persists a replayable extension error and emits its specialized live event.
    async fn report(
        &self,
        extension_id: &ExtensionId,
        point: &'static str,
        error_message: &str,
    ) {
        // Handler failures remain non-fatal, but failures in their diagnostic
        // path must remain observable even when durable replay is unavailable.
        let active_run = match self.execution.active_run() {
            Ok(active_run) => active_run,
            Err(error) => {
                tracing::warn!(
                    "extension diagnostic active Run snapshot failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Identity,
                    error
                );
                None
            }
        };
        let turn_id = match active_run.as_ref() {
            Some(active_run) => active_run.turn_id.clone(),
            None => {
                match TurnId::try_from(self.id_generator.next(IdKind::Turn)) {
                    Ok(turn_id) => turn_id,
                    Err(error) => {
                        tracing::error!(
                            "extension diagnostic Turn ID generation failed for extension {} at {} during {}: {}",
                            extension_id,
                            point,
                            DiagnosticStage::Identity,
                            error
                        );
                        return;
                    }
                }
            }
        };
        let timestamp = self.clock.now();
        let message_id = match protocol::MessageId::try_from(
            self.id_generator.next(IdKind::Message),
        ) {
            Ok(message_id) => message_id,
            Err(error) => {
                tracing::error!(
                    "extension diagnostic message ID generation failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Identity,
                    error
                );
                return;
            }
        };
        let message = protocol::AgentMessage {
            identity: MessageIdentity {
                message_id,
                turn_id: turn_id.clone(),
            },
            timing: protocol::MessageTiming {
                timestamp_ms: timestamp,
                started_at_ms: timestamp,
                ended_at_ms: timestamp,
            },
            content: protocol::MessageContent::Extension {
                extension: protocol::ExtensionMessage {
                    extension_id: extension_id.clone(),
                    custom_type: point.to_string(),
                    blocks: vec![ContentBlock::Text {
                        text: error_message.to_string(),
                    }],
                    display: true,
                    include_in_context: false,
                    details: Some(serde_json::json!({
                        "kind": "handler_error",
                        "point": point,
                    })),
                },
            },
        };
        let entry_id = match protocol::EntryId::try_from(
            self.id_generator.next(IdKind::Entry),
        ) {
            Ok(entry_id) => entry_id,
            Err(error) => {
                tracing::error!(
                    "extension diagnostic entry ID generation failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Identity,
                    error
                );
                return;
            }
        };
        let _persisted = match self
            .transcript
            .append_context_message(entry_id, &message)
        {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(
                    "extension diagnostic persistence failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Persist,
                    error
                );
                false
            }
        };
        // Live delivery remains independent from persistence so one failing
        // diagnostic path does not hide failures in the other infrastructure.
        let sink = active_run.map(|active_run| Arc::clone(&active_run.sink));
        let Some(sink) = sink else {
            return;
        };
        let sequence = match self.execution.next_sequence() {
            Ok(sequence) => sequence,
            Err(error) => {
                tracing::error!(
                    "extension diagnostic sequence failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Sequence,
                    error
                );
                return;
            }
        };
        if let Err(error) = sink
            .emit(protocol::AgentEvent {
                metadata: protocol::EventMetadata {
                    turn_id,
                    timestamp_ms: self.clock.now(),
                    sequence,
                },
                payload: protocol::AgentEventPayload::ExtensionHandlerFailed {
                    extension_id: extension_id.clone(),
                    point: point.to_string(),
                    message: error_message.to_string(),
                },
            })
            .await
        {
            tracing::error!(
                "extension diagnostic event emission failed for extension {} at {} during {}: {}",
                extension_id,
                point,
                DiagnosticStage::Sink,
                error
            );
        }
    }
}
