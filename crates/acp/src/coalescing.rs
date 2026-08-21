use agent_client_protocol::{RawJsonRpcMessage, RawJsonRpcParams};
use protocol::ProductIdentity;

/// Recovery metadata field carrying the inclusive merged sequence endpoint.
const LAST_SEQUENCE_FIELD: &str = "lastSequence";

/// Supported streaming channels that can share one transport envelope.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkKind {
    Message,
    Thought,
}

/// Stable ACP message identity required for adjacent chunk merging.
#[derive(PartialEq, Eq)]
struct MessageKey<'a> {
    session_id: &'a str,
    message_id: &'a str,
    annotations: Option<&'a serde_json::Value>,
}

/// Recovery identity that must remain unchanged across one merged chunk.
#[derive(PartialEq, Eq)]
struct RecoveryKey<'a> {
    run_id: &'a str,
    turn_id: &'a str,
}

/// Complete equality key for one merge-compatible streaming text block.
#[derive(PartialEq, Eq)]
struct ChunkKey<'a> {
    kind: ChunkKind,
    message: MessageKey<'a>,
    recovery: RecoveryKey<'a>,
}

/// Inclusive Kernel sequence range represented by one outbound notification.
struct SequenceRange {
    first: u64,
    last: u64,
}

/// Borrowed semantic view over one eligible ACP streaming notification.
struct StreamChunk<'a> {
    key: ChunkKey<'a>,
    sequence: SequenceRange,
    text: &'a str,
}

impl<'a> StreamChunk<'a> {
    /// Parses only stream chunks whose projection metadata makes merging safe.
    fn from_message(message: &'a RawJsonRpcMessage) -> Option<Self> {
        let RawJsonRpcMessage::Notification(notification) = message else {
            return None;
        };
        if notification.method.as_ref() != "session/update" {
            return None;
        }
        let RawJsonRpcParams::Object(params) = notification.params.as_ref()?
        else {
            return None;
        };
        let session_id = params.get("sessionId")?.as_str()?;
        let update = params.get("update")?.as_object()?;
        let kind = match update.get("sessionUpdate")?.as_str()? {
            "agent_message_chunk" => ChunkKind::Message,
            "agent_thought_chunk" => ChunkKind::Thought,
            _ => return None,
        };
        let message_id = update.get("messageId")?.as_str()?;
        let content = update.get("content")?.as_object()?;
        if content.get("type")?.as_str()? != "text" {
            return None;
        }
        let text = content.get("text")?.as_str()?;
        let annotations = content.get("annotations");
        let product = params
            .get("_meta")?
            .as_object()?
            .get(ProductIdentity::ACP_NAMESPACE)?
            .as_object()?;
        let turn_id = product.get("turnId")?.as_str()?;
        let first = product.get("sequence")?.as_u64()?;
        let group = product.get("sessionRecovery")?.as_object()?;
        if group.get("projectionIndex")?.as_u64()? != 0
            || group.get("projectionCount")?.as_u64()? != 1
            || group.contains_key("operationPhase")
        {
            return None;
        }
        let run_id = group.get("runId")?.as_str()?;
        let last = match group.get(LAST_SEQUENCE_FIELD) {
            Some(last) => last.as_u64()?,
            None => first,
        };
        if first == 0 || last < first {
            return None;
        }
        Some(Self {
            key: ChunkKey {
                kind,
                message: MessageKey {
                    session_id,
                    message_id,
                    annotations,
                },
                recovery: RecoveryKey { run_id, turn_id },
            },
            sequence: SequenceRange { first, last },
            text,
        })
    }
}

/// Appends one validated chunk and records the inclusive sequence endpoint.
fn append_chunk(
    message: &mut RawJsonRpcMessage,
    text: &str,
    last_sequence: u64,
) {
    let RawJsonRpcMessage::Notification(notification) = message else {
        unreachable!("eligible stream chunk must remain a notification");
    };
    let Some(RawJsonRpcParams::Object(params)) = notification.params.as_mut()
    else {
        unreachable!("eligible stream chunk must retain named params");
    };
    let Some(recovery) = params
        .get_mut("_meta")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|meta| meta.get_mut(ProductIdentity::ACP_NAMESPACE))
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|product| product.get_mut("sessionRecovery"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        unreachable!("eligible stream chunk must retain recovery metadata");
    };
    recovery.insert(
        LAST_SEQUENCE_FIELD.to_string(),
        serde_json::json!(last_sequence),
    );
    let Some(serde_json::Value::String(target)) = params
        .get_mut("update")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|update| update.get_mut("content"))
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|content| content.get_mut("text"))
    else {
        unreachable!("eligible stream chunk must retain text content");
    };
    target.push_str(text);
}

/// Merges one adjacent source chunk into the prior outbound notification.
fn merge_adjacent(
    target: &mut RawJsonRpcMessage,
    source: &RawJsonRpcMessage,
) -> bool {
    let (text, last_sequence) = {
        let Some(target_chunk) = StreamChunk::from_message(target) else {
            return false;
        };
        let Some(source_chunk) = StreamChunk::from_message(source) else {
            return false;
        };
        if target_chunk.key != source_chunk.key
            || source_chunk.sequence.first <= target_chunk.sequence.last
        {
            return false;
        }
        (source_chunk.text.to_owned(), source_chunk.sequence.last)
    };
    // Both messages were validated immediately above, so mutation cannot
    // partially succeed and then fall back to emitting the source again.
    append_chunk(target, &text, last_sequence);
    true
}

/// Coalesces only adjacent eligible ACP streaming chunks in source order.
pub(crate) fn coalesce_updates(
    messages: Vec<RawJsonRpcMessage>,
) -> Vec<RawJsonRpcMessage> {
    let mut output = Vec::with_capacity(messages.len());
    for message in messages {
        if output
            .last_mut()
            .is_some_and(|target| merge_adjacent(target, &message))
        {
            continue;
        }
        output.push(message);
    }
    output
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v2 as wire;
    use agent_client_protocol::{RawJsonRpcMessage, RawJsonRpcParams};

    use super::coalesce_updates;

    /// Builds one real ACP message chunk with complete recovery metadata.
    fn chunk(sequence: u64, text: &str) -> RawJsonRpcMessage {
        let event_metadata = wire::Meta::from_iter([(
            "clawcode".to_string(),
            serde_json::json!({
                "turnId": "turn-1",
                "timestampMs": sequence.to_string(),
                "sequence": sequence
            }),
        )]);
        let recovery_metadata = wire::Meta::from_iter([(
            "clawcode".to_string(),
            serde_json::json!({
                "turnId": "turn-1",
                "timestampMs": sequence.to_string(),
                "sequence": sequence,
                "sessionRecovery": {
                    "runId": "run-1",
                    "projectionIndex": 0,
                    "projectionCount": 1
                }
            }),
        )]);
        let update = wire::SessionUpdate::AgentMessageChunk(
            wire::ContentChunk::new(
                wire::ContentBlock::Text(
                    wire::TextContent::new(text).meta(event_metadata.clone()),
                ),
                "message-1",
            )
            .meta(event_metadata),
        );
        let notification =
            wire::UpdateSessionNotification::new("session-1", update)
                .meta(recovery_metadata);
        RawJsonRpcMessage::notification(
            "session/update".to_string(),
            serde_json::to_value(notification).expect("serialize notification"),
        )
        .expect("valid raw notification")
    }

    /// Returns mutable named params from one raw test notification.
    fn params_mut(
        message: &mut RawJsonRpcMessage,
    ) -> &mut serde_json::Map<String, serde_json::Value> {
        let RawJsonRpcMessage::Notification(notification) = message else {
            panic!("expected Session notification");
        };
        let Some(RawJsonRpcParams::Object(params)) =
            notification.params.as_mut()
        else {
            panic!("expected named notification params");
        };
        params
    }

    /// Builds one thought chunk by changing only the standard update variant.
    fn thought_chunk(sequence: u64, text: &str) -> RawJsonRpcMessage {
        let mut message = chunk(sequence, text);
        params_mut(&mut message)["update"]["sessionUpdate"] =
            serde_json::json!("agent_thought_chunk");
        message
    }

    /// Builds one ToolCall update that must remain a transport boundary.
    fn tool_call() -> RawJsonRpcMessage {
        RawJsonRpcMessage::notification(
            "session/update".to_string(),
            serde_json::json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tool-1",
                    "status": "in_progress"
                }
            }),
        )
        .expect("valid ToolCall notification")
    }

    /// Decodes one raw Session update for behavior assertions.
    fn decode(message: &RawJsonRpcMessage) -> wire::UpdateSessionNotification {
        let RawJsonRpcMessage::Notification(notification) = message else {
            panic!("expected Session notification");
        };
        let params = notification
            .params
            .clone()
            .map_or(serde_json::Value::Null, RawJsonRpcParams::into_value);
        serde_json::from_value(params).expect("decode Session notification")
    }

    /// Adjacent chunks for one Agent message share one transport envelope.
    #[test]
    fn coalesces_agent_message_chunks() {
        let messages =
            coalesce_updates(vec![chunk(41, "Hel"), chunk(42, "lo")]);

        assert_eq!(messages.len(), 1);
        let notification = decode(&messages[0]);
        let wire::SessionUpdate::AgentMessageChunk(chunk) = notification.update
        else {
            panic!("expected Agent message chunk");
        };
        let wire::ContentBlock::Text(content) = chunk.content else {
            panic!("expected text content");
        };
        assert_eq!(content.text, "Hello");
        let product = notification
            .meta
            .as_ref()
            .and_then(|meta| meta.get("clawcode"))
            .and_then(serde_json::Value::as_object)
            .expect("product metadata");
        assert_eq!(product.get("sequence"), Some(&serde_json::json!(41)));
        assert_eq!(
            product
                .get("sessionRecovery")
                .and_then(serde_json::Value::as_object)
                .and_then(|recovery| recovery.get("lastSequence")),
            Some(&serde_json::json!(42))
        );
    }

    /// Adjacent thought chunks use the same envelope optimization independently.
    #[test]
    fn coalesces_agent_thought_chunks() {
        let messages = coalesce_updates(vec![
            thought_chunk(51, "rea"),
            thought_chunk(52, "son"),
        ]);

        assert_eq!(messages.len(), 1);
        let notification = decode(&messages[0]);
        let wire::SessionUpdate::AgentThoughtChunk(chunk) = notification.update
        else {
            panic!("expected Agent thought chunk");
        };
        let wire::ContentBlock::Text(content) = chunk.content else {
            panic!("expected text content");
        };
        assert_eq!(content.text, "reason");
    }

    /// Different message identities never share a streaming envelope.
    #[test]
    fn does_not_merge_different_message_ids() {
        let first = chunk(61, "first");
        let mut second = chunk(62, "second");
        params_mut(&mut second)["update"]["messageId"] =
            serde_json::json!("message-2");

        assert_eq!(coalesce_updates(vec![first, second]).len(), 2);
    }

    /// Visible message and thought streams remain distinct projection channels.
    #[test]
    fn does_not_merge_message_and_thought_chunks() {
        assert_eq!(
            coalesce_updates(vec![
                chunk(71, "answer"),
                thought_chunk(72, "reason")
            ])
            .len(),
            2
        );
    }

    /// Non-text content is never rewritten by the text-only optimization.
    #[test]
    fn does_not_merge_non_text_content() {
        let first = chunk(81, "text");
        let mut second = chunk(82, "ignored");
        params_mut(&mut second)["update"]["content"] = serde_json::json!({
            "type": "resource_link",
            "name": "resource",
            "uri": "file:///tmp/resource"
        });

        assert_eq!(coalesce_updates(vec![first, second]).len(), 2);
    }

    /// Distinct text annotations retain their original notification boundaries.
    #[test]
    fn does_not_merge_different_annotations() {
        let first = chunk(86, "first");
        let mut second = chunk(87, "second");
        params_mut(&mut second)["update"]["content"]["annotations"] =
            serde_json::json!({ "audience": ["assistant"] });

        assert_eq!(coalesce_updates(vec![first, second]).len(), 2);
    }

    /// Multi-update Kernel projections retain their original atomic group shape.
    #[test]
    fn does_not_merge_multi_update_projection_groups() {
        let first = chunk(91, "first");
        let mut second = chunk(92, "second");
        params_mut(&mut second)["_meta"]["clawcode"]["sessionRecovery"]["projectionCount"] =
            serde_json::json!(2);

        assert_eq!(coalesce_updates(vec![first, second]).len(), 2);
    }

    /// Operation boundaries cannot disappear into a streaming chunk envelope.
    #[test]
    fn does_not_merge_operation_boundaries() {
        let first = chunk(101, "first");
        let mut second = chunk(102, "second");
        params_mut(&mut second)["_meta"]["clawcode"]["sessionRecovery"]["operationPhase"] =
            serde_json::json!("end");

        assert_eq!(coalesce_updates(vec![first, second]).len(), 2);
    }

    /// ToolCall updates flush both neighboring streaming segments in source order.
    #[test]
    fn tool_call_update_breaks_chunk_coalescing() {
        let messages = coalesce_updates(vec![
            chunk(111, "before"),
            tool_call(),
            chunk(112, "after"),
        ]);

        assert_eq!(messages.len(), 3);
        assert!(matches!(
            decode(&messages[1]).update,
            wire::SessionUpdate::ToolCallUpdate(_)
        ));
    }
}
