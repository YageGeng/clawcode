use agent_client_protocol::schema::v2 as wire;
use protocol::{RunId, Sequence, SessionId};
use serde::{Deserialize, Serialize};

/// ACP replay cursor discriminator owned by the ClawCode recovery extension.
pub(crate) const RECOVERY_CURSOR_TYPE: &str = "_clawcode/event";

/// Stable diagnostic code telling a client to discard its incremental cursor.
pub(crate) const CURSOR_UNAVAILABLE_CODE: &str =
    "_clawcode/session_recovery_cursor_unavailable";

/// Recovery protocol version supported by this ACP adapter.
pub(crate) const RECOVERY_VERSION: u32 = 1;

/// One Session cursor submitted during ACP initialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionCursor {
    /// Session whose in-memory projection the client retained.
    pub(crate) session_id: SessionId,
    /// Operation that owns the retained projection.
    pub(crate) run_id: RunId,
    /// First Kernel sequence the client has not atomically applied.
    pub(crate) next_sequence: Sequence,
}

/// ClawCode recovery capabilities and cursors declared by one ACP client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryRequest {
    /// Recovery protocol version understood by the client.
    pub(crate) version: u32,
    /// Session projections retained across the disconnected transport.
    #[serde(default)]
    pub(crate) sessions: Vec<SessionCursor>,
}

/// Recovery action selected by the server during ACP initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryMode {
    /// Preserve the client projection and incrementally attach to its journal.
    Watch,
    /// Clear the client projection and replay the Session from its start.
    Resume,
}

/// One server-selected recovery plan returned from ACP initialization.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryPlan {
    /// Session to recover after initialization completes.
    pub(crate) session_id: SessionId,
    /// Incremental watch or full Resume decision.
    pub(crate) mode: RecoveryMode,
    /// Recoverable Operation when mode is watch.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub(crate) run_id: Option<RunId>,
    /// First unapplied event when mode is watch.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub(crate) next_sequence: Option<Sequence>,
    /// Runtime state observed while selecting this plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub(crate) running: Option<bool>,
}

/// Recovery plan collection returned in ACP initialization metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryResponse {
    /// Recovery protocol version selected by the server.
    pub(crate) version: u32,
    /// Per-Session actions the client must perform after Session discovery.
    pub(crate) sessions: Vec<RecoveryPlan>,
}

/// Diagnostic metadata describing one completed standard ACP Resume request.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumeMeta {
    /// Recovery path used by the server.
    pub(crate) mode: RecoveryMode,
    /// Operation journal used for projection recovery, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub(crate) run_id: Option<RunId>,
    /// Last Kernel sequence currently retained in that journal.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub(crate) journal_tail_sequence: Option<Sequence>,
    /// Runtime state observed when the watcher was attached.
    pub(crate) running: bool,
}

impl RecoveryRequest {
    /// Reads optional recovery metadata from one ACP initialization envelope.
    pub(crate) fn from_meta(
        meta: Option<&wire::Meta>,
        namespace: &str,
    ) -> Result<Option<Self>, AcpRecoveryError> {
        let Some(product) = meta.and_then(|meta| meta.get(namespace)) else {
            return Ok(None);
        };
        let Some(recovery) = product
            .as_object()
            .and_then(|product| product.get("sessionRecovery"))
        else {
            return Ok(None);
        };
        serde_json::from_value(recovery.clone())
            .map(Some)
            .map_err(|error| {
                AcpRecoveryError::InvalidInitialize(error.to_string())
            })
    }
}

/// Operation boundary carried by every notification in a projected event group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationPhase {
    /// This complete group starts a recoverable Operation journal.
    Start,
    /// This complete group finishes the recoverable Operation journal.
    End,
}

/// Recovery metadata shared by notifications mapped from one Kernel event.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupMeta {
    /// Operation that owns this projected event group.
    pub(crate) run_id: RunId,
    /// Zero-based mapper position within the group.
    pub(crate) projection_index: usize,
    /// Total number of mapper outputs in the group.
    pub(crate) projection_count: usize,
    /// Explicit Operation boundary when this group starts or ends a journal.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub(crate) operation_phase: Option<OperationPhase>,
}

/// Structured JSON-RPC error data for an unavailable incremental cursor.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CursorErrorData {
    /// Stable machine-readable recovery failure code.
    #[builder(default = CURSOR_UNAVAILABLE_CODE)]
    pub(crate) code: &'static str,
    /// Session whose retained projection can no longer be reused.
    pub(crate) session_id: SessionId,
    /// Operation requested by the stale cursor.
    pub(crate) run_id: RunId,
    /// Inclusive sequence requested by the stale cursor.
    pub(crate) sequence: Sequence,
}

/// A validated inclusive cursor into one ACP Operation projection journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EventCursor {
    /// Operation whose projection journal owns the sequence.
    pub(crate) run_id: RunId,
    /// First Kernel event sequence that must be replayed.
    pub(crate) sequence: Sequence,
}

/// Replay behavior requested through the standard ACP Resume method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpReplayRequest {
    /// Attach without replaying prior updates.
    None,
    /// Replay the complete Session projection.
    Start,
    /// Replay one ClawCode Operation from an inclusive event cursor.
    Event(EventCursor),
}

/// Invalid recovery metadata received at the ACP protocol boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AcpRecoveryError {
    /// The client supplied an unsupported implementation cursor.
    #[error("unsupported session replay cursor {0}")]
    UnsupportedCursor(String),
    /// The client omitted or malformed one required cursor field.
    #[error("invalid session replay cursor: {0}")]
    InvalidCursor(String),
    /// The client declared malformed initialization recovery metadata.
    #[error("invalid initialize session recovery metadata: {0}")]
    InvalidInitialize(String),
}

impl TryFrom<Option<wire::ReplayFrom>> for AcpReplayRequest {
    type Error = AcpRecoveryError;

    /// Validates standard and ClawCode-specific ACP replay cursors.
    fn try_from(
        replay_from: Option<wire::ReplayFrom>,
    ) -> Result<Self, Self::Error> {
        match replay_from {
            None => Ok(Self::None),
            Some(wire::ReplayFrom::Start(_)) => Ok(Self::Start),
            Some(wire::ReplayFrom::Other(mut cursor)) => {
                if cursor.type_ != RECOVERY_CURSOR_TYPE {
                    return Err(AcpRecoveryError::UnsupportedCursor(
                        cursor.type_,
                    ));
                }
                let run_id =
                    cursor.fields.remove("runId").ok_or_else(|| {
                        AcpRecoveryError::InvalidCursor(
                            "missing runId".to_string(),
                        )
                    })?;
                let sequence =
                    cursor.fields.remove("sequence").ok_or_else(|| {
                        AcpRecoveryError::InvalidCursor(
                            "missing sequence".to_string(),
                        )
                    })?;
                Ok(Self::Event(EventCursor {
                    run_id: serde_json::from_value(run_id).map_err(
                        |error| {
                            AcpRecoveryError::InvalidCursor(format!(
                                "invalid runId: {error}"
                            ))
                        },
                    )?,
                    sequence: serde_json::from_value(sequence).map_err(
                        |error| {
                            AcpRecoveryError::InvalidCursor(format!(
                                "invalid sequence: {error}"
                            ))
                        },
                    )?,
                }))
            }
            Some(_) => {
                Err(AcpRecoveryError::UnsupportedCursor("unknown".to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use agent_client_protocol::schema::v2 as wire;
    use protocol::{RunId, Sequence, SessionId};

    use super::{
        AcpReplayRequest, CursorErrorData, EventCursor, GroupMeta,
        OperationPhase, RecoveryMode, RecoveryPlan, RecoveryRequest,
        RecoveryResponse, ResumeMeta,
    };

    /// The ClawCode event cursor preserves the requested Run and inclusive sequence.
    #[test]
    fn parses_clawcode_event_replay_cursor() {
        let cursor = wire::OtherReplayFrom::new(
            "_clawcode/event",
            BTreeMap::from([
                (
                    "runId".to_string(),
                    serde_json::Value::String("run-1".to_string()),
                ),
                ("sequence".to_string(), serde_json::json!(24)),
            ]),
        );

        let parsed =
            AcpReplayRequest::try_from(Some(wire::ReplayFrom::Other(cursor)))
                .expect("parse ClawCode event cursor");

        assert_eq!(
            parsed,
            AcpReplayRequest::Event(EventCursor {
                run_id: RunId::try_from("run-1").expect("Run id"),
                sequence: Sequence::try_from(24_u64).expect("sequence"),
            })
        );
    }

    /// Unknown custom replay cursors are rejected instead of guessing recovery semantics.
    #[test]
    fn rejects_unknown_replay_cursor() {
        let cursor = wire::OtherReplayFrom::new("future", BTreeMap::new());

        let error =
            AcpReplayRequest::try_from(Some(wire::ReplayFrom::Other(cursor)))
                .expect_err("unknown cursor must fail");

        assert_eq!(
            error.to_string(),
            "unsupported session replay cursor future"
        );
    }

    /// Initialize recovery metadata preserves every submitted Session cursor.
    #[test]
    fn parses_initialize_recovery_cursors() {
        let meta = wire::Meta::from_iter([(
            "clawcode".to_string(),
            serde_json::json!({
                "sessionRecovery": {
                    "version": 1,
                    "sessions": [{
                        "sessionId": "session-1",
                        "runId": "run-1",
                        "nextSequence": 24
                    }]
                }
            }),
        )]);

        let request = RecoveryRequest::from_meta(Some(&meta), "clawcode")
            .expect("parse Initialize recovery metadata")
            .expect("declared recovery extension");

        assert_eq!(request.version, 1);
        assert_eq!(request.sessions.len(), 1);
        let cursor = request.sessions.first().expect("Session cursor");
        assert_eq!(cursor.session_id.as_str(), "session-1");
        assert_eq!(cursor.run_id.as_str(), "run-1");
        assert_eq!(cursor.next_sequence.get(), 24);
    }

    /// Initialize remains compatible when the client omits recovery metadata.
    #[test]
    fn accepts_initialize_without_recovery_metadata() {
        let meta = wire::Meta::from_iter([(
            "clawcode".to_string(),
            serde_json::json!({ "methods": [] }),
        )]);

        assert_eq!(
            RecoveryRequest::from_meta(Some(&meta), "clawcode")
                .expect("parse compatible metadata"),
            None
        );
    }

    /// Projection metadata serializes explicit Operation boundaries.
    #[test]
    fn serializes_projection_operation_phase() {
        let metadata = GroupMeta::builder()
            .run_id(RunId::try_from("run-1").expect("Run id"))
            .projection_index(0)
            .projection_count(2)
            .operation_phase(OperationPhase::Start)
            .build();

        assert_eq!(
            serde_json::to_value(metadata)
                .expect("serialize projection metadata"),
            serde_json::json!({
                "runId": "run-1",
                "projectionIndex": 0,
                "projectionCount": 2,
                "operationPhase": "start"
            })
        );
    }

    /// Cursor-unavailable data gives clients a stable full-replay signal.
    #[test]
    fn serializes_cursor_unavailable_error_data() {
        let data = CursorErrorData::builder()
            .session_id(SessionId::try_from("session-1").expect("Session id"))
            .run_id(RunId::try_from("run-1").expect("Run id"))
            .sequence(Sequence::try_from(24_u64).expect("sequence"))
            .build();

        assert_eq!(
            serde_json::to_value(data).expect("serialize cursor error data"),
            serde_json::json!({
                "code": "_clawcode/session_recovery_cursor_unavailable",
                "sessionId": "session-1",
                "runId": "run-1",
                "sequence": 24
            })
        );
    }

    /// Initialize plans serialize watch and full-resume decisions without ambiguity.
    #[test]
    fn serializes_initialize_recovery_plans() {
        let response = RecoveryResponse {
            version: 1,
            sessions: vec![
                RecoveryPlan::builder()
                    .session_id(
                        SessionId::try_from("session-1").expect("Session id"),
                    )
                    .mode(RecoveryMode::Watch)
                    .run_id(RunId::try_from("run-1").expect("Run id"))
                    .next_sequence(
                        Sequence::try_from(24_u64).expect("sequence"),
                    )
                    .running(true)
                    .build(),
            ],
        };

        assert_eq!(
            serde_json::to_value(response).expect("serialize recovery plans"),
            serde_json::json!({
                "version": 1,
                "sessions": [{
                    "sessionId": "session-1",
                    "mode": "watch",
                    "runId": "run-1",
                    "nextSequence": 24,
                    "running": true
                }]
            })
        );
    }

    /// Resume diagnostics report actual behavior without becoming a client cursor.
    #[test]
    fn serializes_resume_recovery_diagnostics() {
        let metadata = ResumeMeta::builder()
            .mode(RecoveryMode::Resume)
            .run_id(RunId::try_from("run-1").expect("Run id"))
            .journal_tail_sequence(
                Sequence::try_from(40_u64).expect("sequence"),
            )
            .running(false)
            .build();

        assert_eq!(
            serde_json::to_value(metadata).expect("serialize Resume metadata"),
            serde_json::json!({
                "mode": "resume",
                "runId": "run-1",
                "journalTailSequence": 40,
                "running": false
            })
        );
    }
}
