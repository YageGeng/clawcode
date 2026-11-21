//! Kernel events carried by `_clawcode/event` updates, decoded once into the
//! protocol's typed payload and shared by the transcript and the controls.

use agent_client_protocol::v2::schema as wire;
use protocol::{AgentEventPayload, ProductIdentity};
use serde::Deserialize as _;
use serde_json::Value;

use super::ProductMeta;

/// One decoded kernel event and the product metadata of its update.
#[derive(Debug)]
pub struct ProductEvent {
    /// The typed payload.
    pub payload: AgentEventPayload,
    /// Turn, time, and sequence of the event.
    pub meta: ProductMeta,
}

impl ProductEvent {
    /// Decodes the events the desktop reads; every other event is `None`.
    ///
    /// `message_end` is decoded only for extension messages, the handler
    /// errors and question replies read here, so replaying a long session does
    /// not decode every assistant message. `usage_updated` is never decoded:
    /// the server re-encodes its counters as strings, which the typed usage
    /// does not accept, so the controls read it from the raw payload.
    pub fn decode(update: &wire::OtherSessionUpdate) -> Option<Self> {
        if update.session_update != ProductIdentity::ACP_EVENT_UPDATE {
            return None;
        }
        let payload = update.fields.get("payload")?;
        let wanted = match payload.get("event").and_then(Value::as_str)? {
            "queue_changed"
            | "retry_scheduled"
            | "retry_start"
            | "retry_end"
            | "extension_handler_failed" => true,
            "message_end" => {
                payload
                    .pointer("/message/content/type")
                    .and_then(Value::as_str)
                    == Some("extension")
            }
            _ => false,
        };
        if !wanted {
            return None;
        }
        let payload = match AgentEventPayload::deserialize(payload) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!("ignored malformed kernel event: {}", error);
                return None;
            }
        };
        let meta = update
            .fields
            .get("_meta")
            .and_then(Value::as_object)
            .map(ProductMeta::from)
            .unwrap_or_default();
        Some(ProductEvent { payload, meta })
    }
}
