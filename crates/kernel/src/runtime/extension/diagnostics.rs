use std::sync::Arc;

use async_trait::async_trait;
use extension::ExtensionDiagnosticSink;
use protocol::{
    ContentBlock, ExtensionId, IdGenerator, IdKind, LaneId, MessageIdentity,
    TurnId,
};
use store::{EntryKind, NewEntry, SessionStore};

/// Failure stage for infrastructure that records extension handler diagnostics.
#[derive(Debug)]
enum DiagnosticStage {
    Identity,
    Serialize,
    Persist,
    History,
    Sequence,
    Sink,
}

impl std::fmt::Display for DiagnosticStage {
    /// Formats a stable stage value for readable diagnostic messages.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "identity",
            Self::Serialize => "serialize",
            Self::Persist => "persist",
            Self::History => "history",
            Self::Sequence => "sequence",
            Self::Sink => "sink",
        })
    }
}

/// Session-owned diagnostic sink that projects sanitized handler failures live.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct KernelExtensionDiagnostics {
    clock: Arc<dyn store::Clock>,
    sink: Arc<std::sync::Mutex<Option<Arc<dyn crate::EventSink>>>>,
    turn_id: Arc<std::sync::Mutex<Option<TurnId>>>,
    sequence: Arc<std::sync::atomic::AtomicU64>,
    store: Arc<std::sync::Mutex<Box<dyn SessionStore>>>,
    history: Arc<std::sync::Mutex<Vec<protocol::AgentMessage>>>,
    lane: LaneId,
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
        let turn_id = match self.turn_id.lock() {
            Ok(turn_id) => turn_id.clone(),
            Err(error) => {
                tracing::warn!(
                    "extension diagnostic turn lock failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Identity,
                    error
                );
                None
            }
        };
        let turn_id = match turn_id {
            Some(turn_id) => turn_id,
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
        let payload = match serde_json::to_value(&message) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(
                    "extension diagnostic serialization failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Serialize,
                    error
                );
                return;
            }
        };
        let persisted = match self.store.lock() {
            Ok(mut store) => match store.append_entry(
                &self.lane,
                NewEntry {
                    id: entry_id,
                    kind: EntryKind::Message,
                    payload,
                },
            ) {
                Ok(_entry) => true,
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
            },
            Err(error) => {
                tracing::error!(
                    "extension diagnostic store lock failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Persist,
                    error
                );
                false
            }
        };
        if persisted {
            match self.history.lock() {
                Ok(mut history) => history.push(message),
                Err(error) => tracing::error!(
                    "extension diagnostic history lock failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::History,
                    error
                ),
            }
        }

        let sink = match self.sink.lock() {
            Ok(sink) => sink.as_ref().map(Arc::clone),
            Err(error) => {
                tracing::error!(
                    "extension diagnostic sink lock failed for extension {} at {} during {}: {}",
                    extension_id,
                    point,
                    DiagnosticStage::Sink,
                    error
                );
                return;
            }
        };
        let Some(sink) = sink else {
            return;
        };
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .checked_add(1)
            .ok_or("event sequence overflow")
            .and_then(|value| {
                protocol::Sequence::try_from(value)
                    .map_err(|_error| "event sequence is invalid")
            });
        let sequence = match sequence {
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
