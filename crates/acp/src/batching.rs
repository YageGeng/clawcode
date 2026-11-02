use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::{
    Agent, Channel, Client, ConnectTo, ConnectionDriver, RawJsonRpcMessage,
    RawJsonRpcParams, TransportFrame,
};
use futures::channel::mpsc;
use futures::{FutureExt as _, StreamExt as _};
use protocol::ProductIdentity;

use crate::coalescing::coalesce_updates;

/// Product metadata key that declares the capability and carries batched updates.
const BATCH_KEY: &str = "sessionUpdateBatch";
/// Carrier format version this adapter understands and emits.
const BATCH_VERSION: u64 = 1;

/// ACP component wrapper that batches queued outbound Session notifications.
pub(crate) struct BatchComponent<C> {
    inner: C,
    max_batch_size: NonZeroUsize,
}

impl<C> BatchComponent<C> {
    /// Wraps one ACP component with the configured outbound batch limit.
    pub(crate) fn new(inner: C, max_batch_size: NonZeroUsize) -> Self {
        Self {
            inner,
            max_batch_size,
        }
    }
}

/// Returns the Session id of a standard `session/update` notification.
fn session_update_id(message: &RawJsonRpcMessage) -> Option<&str> {
    let RawJsonRpcMessage::Notification(notification) = message else {
        return None;
    };
    if notification.method.as_ref() != "session/update" {
        return None;
    }
    match notification.params.as_ref()? {
        RawJsonRpcParams::Object(params) => params.get("sessionId")?.as_str(),
        RawJsonRpcParams::Array(_) => None,
    }
}

/// Returns one notification's product metadata object, creating absent levels.
///
/// Returns `None` when `_meta` or the product namespace holds a non-object
/// value, which cannot carry the batch key without discarding client data.
fn product_meta(
    message: &mut RawJsonRpcMessage,
) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
    let RawJsonRpcMessage::Notification(notification) = message else {
        return None;
    };
    let Some(RawJsonRpcParams::Object(params)) = notification.params.as_mut()
    else {
        return None;
    };
    let meta = params.entry("_meta").or_insert(serde_json::Value::Null);
    // ACP treats an omitted and a null `_meta` the same way.
    if meta.is_null() {
        *meta = serde_json::Value::Object(serde_json::Map::new());
    }
    let product = meta
        .as_object_mut()?
        .entry(ProductIdentity::ACP_NAMESPACE)
        .or_insert(serde_json::Value::Null);
    if product.is_null() {
        *product = serde_json::Value::Object(serde_json::Map::new());
    }
    product.as_object_mut()
}

/// Records whether this connection's client accepts batched Session updates.
struct InboundRelay {
    accepts_batches: Arc<AtomicBool>,
}

impl InboundRelay {
    /// Forwards inbound frames unchanged after observing initialization metadata.
    async fn forward(
        self,
        mut source: mpsc::UnboundedReceiver<TransportFrame>,
        sink: mpsc::UnboundedSender<TransportFrame>,
    ) -> Result<(), agent_client_protocol::Error> {
        while let Some(frame) = source.next().await {
            // The Agent can only emit Session updates after it receives this
            // frame, so the outbound relay always sees the latest capability.
            self.observe(&frame);
            sink.unbounded_send(frame)
                .map_err(agent_client_protocol::Error::into_internal_error)?;
        }
        Ok(())
    }

    /// Re-evaluates the capability from one `initialize` request; other frames are ignored.
    fn observe(&self, frame: &TransportFrame) {
        let TransportFrame::Single(RawJsonRpcMessage::Request(request)) = frame
        else {
            return;
        };
        if request.method.as_ref() != "initialize" {
            return;
        }
        let declared = match request.params.as_ref() {
            Some(RawJsonRpcParams::Object(params)) => {
                params
                    .get("_meta")
                    .and_then(|meta| meta.get(ProductIdentity::ACP_NAMESPACE))
                    .and_then(|product| product.get(BATCH_KEY))
                    .and_then(|capability| capability.get("version"))
                    .and_then(serde_json::Value::as_u64)
                    == Some(BATCH_VERSION)
            }
            Some(RawJsonRpcParams::Array(_)) | None => false,
        };
        tracing::debug!(
            "ACP initialize negotiated batched Session updates: {}",
            declared
        );
        self.accepts_batches.store(declared, Ordering::Release);
    }
}

/// Drains queued same-Session updates into one private carrier when negotiated.
struct OutboundRelay {
    max_batch_size: NonZeroUsize,
    accepts_batches: Arc<AtomicBool>,
}

impl OutboundRelay {
    /// Forwards outbound frames while batching only currently queued Session updates.
    async fn forward(
        self,
        mut source: mpsc::UnboundedReceiver<TransportFrame>,
        sink: mpsc::UnboundedSender<TransportFrame>,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut pending = None;
        loop {
            let frame = match pending.take() {
                Some(frame) => frame,
                None => match source.next().await {
                    Some(frame) => frame,
                    None => return Ok(()),
                },
            };
            let TransportFrame::Single(message) = frame else {
                sink.unbounded_send(frame).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
                continue;
            };
            let Some(session_id) =
                session_update_id(&message).map(str::to_owned)
            else {
                sink.unbounded_send(TransportFrame::Single(message))
                    .map_err(
                        agent_client_protocol::Error::into_internal_error,
                    )?;
                continue;
            };

            let mut messages = vec![message];
            let mut source_closed = false;
            // Stop draining at the configured limit so one replay cannot create an
            // unbounded frame and monopolize browser-side rendering. Only one
            // Session per carrier: HTTP/SSE routes each update by its Session id.
            while messages.len() < self.max_batch_size.get() {
                match source.next().now_or_never() {
                    Some(Some(TransportFrame::Single(message)))
                        if session_update_id(&message)
                            == Some(session_id.as_str()) =>
                    {
                        messages.push(message);
                    }
                    Some(Some(frame)) => {
                        pending = Some(frame);
                        break;
                    }
                    Some(None) => {
                        source_closed = true;
                        break;
                    }
                    None => break,
                }
            }
            for frame in self.frames(messages) {
                sink.unbounded_send(frame).map_err(
                    agent_client_protocol::Error::into_internal_error,
                )?;
            }
            if source_closed {
                return Ok(());
            }
        }
    }

    /// Turns one coalesced same-Session run into a carrier or single frames.
    fn frames(&self, messages: Vec<RawJsonRpcMessage>) -> Vec<TransportFrame> {
        // Coalesce only the messages already drained for this frame so the
        // optimization cannot add a timer or delay the first streaming token.
        let messages = coalesce_updates(messages);
        if messages.len() < 2 || !self.accepts_batches.load(Ordering::Acquire) {
            return messages.into_iter().map(TransportFrame::Single).collect();
        }
        // Take the outer update from the front without shifting the rest.
        let mut messages = messages.into_iter();
        let mut outer = messages.next().expect("at least two updates");
        let Some(product) = product_meta(&mut outer) else {
            return std::iter::once(outer)
                .chain(messages)
                .map(TransportFrame::Single)
                .collect();
        };
        let nested = messages
            .map(|message| {
                let RawJsonRpcMessage::Notification(notification) = message
                else {
                    unreachable!("drained Session updates are notifications");
                };
                notification.params.map_or(
                    serde_json::Value::Null,
                    RawJsonRpcParams::into_value,
                )
            })
            .collect();
        product.insert(BATCH_KEY.to_string(), serde_json::Value::Array(nested));
        vec![TransportFrame::Single(outer)]
    }
}

impl<C> ConnectTo<Client> for BatchComponent<C>
where
    C: ConnectTo<Client>,
{
    /// Connects the wrapped Agent component through the batching channel boundary.
    async fn connect_to(
        self,
        client: impl ConnectTo<Agent>,
    ) -> Result<(), agent_client_protocol::Error> {
        let (channel, component_driver) =
            <Self as ConnectTo<Client>>::into_channel_and_future(self);
        let component_future = async move {
            match component_driver {
                Some(driver) => driver.await,
                None => Ok(()),
            }
        };
        let client_future =
            <Channel as ConnectTo<Client>>::connect_to(channel, client);
        futures::try_join!(component_future, client_future)?;
        Ok(())
    }

    /// Exposes a frame-aware channel and drives both relay directions to completion.
    fn into_channel_and_future(self) -> (Channel, Option<ConnectionDriver>) {
        let (inner_channel, inner_driver) =
            self.inner.into_channel_and_future();
        // An inner endpoint without owned work lives as long as its channel.
        let inner_future = async move {
            match inner_driver {
                Some(driver) => driver.await,
                None => Ok(()),
            }
        };
        let (outer_channel, relay_channel) = Channel::duplex();
        let (inner_rx, inner_tx) = inner_channel.into_parts();
        let (outer_rx, outer_tx) = relay_channel.into_parts();
        // Negotiation is per connection; both directions share one flag.
        let accepts_batches = Arc::new(AtomicBool::new(false));
        let inbound = InboundRelay {
            accepts_batches: Arc::clone(&accepts_batches),
        };
        let outbound = OutboundRelay {
            max_batch_size: self.max_batch_size,
            accepts_batches,
        };
        let relay = ConnectionDriver::new(async move {
            futures::try_join!(
                inner_future,
                inbound.forward(outer_rx, inner_tx),
                outbound.forward(inner_rx, outer_tx),
            )?;
            Ok(())
        });
        (outer_channel, Some(relay))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use agent_client_protocol::{
        Channel, Client, ConnectTo, RawJsonRpcMessage, RawJsonRpcParams,
        TransportFrame,
    };
    use futures::channel::mpsc;
    use futures::{FutureExt as _, StreamExt as _};

    use super::{BatchComponent, InboundRelay, OutboundRelay};

    /// Capability metadata a client sends to accept batched Session updates.
    fn batch_capability() -> serde_json::Value {
        serde_json::json!({ "clawcode": { "sessionUpdateBatch": { "version": 1 } } })
    }

    /// Builds one `initialize` request frame with the given `_meta` value.
    fn initialize(meta: Option<serde_json::Value>) -> TransportFrame {
        let mut params = serde_json::json!({ "protocolVersion": 2 });
        if let Some(meta) = meta {
            params["_meta"] = meta;
        }
        TransportFrame::Single(
            RawJsonRpcMessage::request("initialize".to_string(), params, 1)
                .expect("valid initialize request"),
        )
    }

    /// Builds one ToolCall update for `session` that coalescing never merges.
    fn update(session: &str, index: usize) -> TransportFrame {
        update_with_meta(session, index, None)
    }

    /// Builds one ToolCall update carrying an optional `_meta` value.
    fn update_with_meta(
        session: &str,
        index: usize,
        meta: Option<serde_json::Value>,
    ) -> TransportFrame {
        let mut params = serde_json::json!({
            "sessionId": session,
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": format!("tool-{index}"),
                "status": "in_progress"
            }
        });
        if let Some(meta) = meta {
            params["_meta"] = meta;
        }
        TransportFrame::Single(
            RawJsonRpcMessage::notification(
                "session/update".to_string(),
                params,
            )
            .expect("valid Session update"),
        )
    }

    /// Returns the JSON params of one single notification frame.
    fn params(frame: &TransportFrame) -> serde_json::Value {
        let TransportFrame::Single(RawJsonRpcMessage::Notification(
            notification,
        )) = frame
        else {
            panic!("expected one single notification frame");
        };
        notification
            .params
            .clone()
            .map_or(serde_json::Value::Null, RawJsonRpcParams::into_value)
    }

    /// Returns the nested carrier items of one frame, if it is a carrier.
    fn batch(frame: &TransportFrame) -> Option<Vec<serde_json::Value>> {
        params(frame)["_meta"]["clawcode"]["sessionUpdateBatch"]
            .as_array()
            .cloned()
    }

    /// Lists the ToolCall ids carried by one frame, outer update first.
    fn tool_ids(frame: &TransportFrame) -> Vec<String> {
        std::iter::once(params(frame))
            .chain(batch(frame).unwrap_or_default())
            .map(|item| {
                item["update"]["toolCallId"]
                    .as_str()
                    .expect("ToolCall id")
                    .to_string()
            })
            .collect()
    }

    /// Builds one raw notification for frame-level batching tests.
    fn notification(method: &str, index: usize) -> TransportFrame {
        TransportFrame::Single(
            RawJsonRpcMessage::notification(
                method.to_string(),
                serde_json::json!({ "index": index }),
            )
            .expect("valid notification"),
        )
    }

    /// Builds one recoverable Agent message chunk for relay integration tests.
    fn stream_chunk(sequence: u64, text: &str) -> TransportFrame {
        TransportFrame::Single(
            RawJsonRpcMessage::notification(
                "session/update".to_string(),
                serde_json::json!({
                    "sessionId": "session-1",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "messageId": "message-1",
                        "content": { "type": "text", "text": text }
                    },
                    "_meta": {
                        "clawcode": {
                            "turnId": "turn-1",
                            "timestampMs": sequence.to_string(),
                            "sequence": sequence,
                            "sessionRecovery": {
                                "runId": "run-1",
                                "projectionIndex": 0,
                                "projectionCount": 1
                            }
                        }
                    }
                }),
            )
            .expect("valid stream notification"),
        )
    }

    /// Builds one ToolCall update that must remain between streaming segments.
    fn tool_call_update() -> TransportFrame {
        TransportFrame::Single(
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
            .expect("valid ToolCall notification"),
        )
    }

    /// Reads text and an optional merged sequence endpoint from one stream frame.
    fn stream_chunk_data(frame: &TransportFrame) -> (&str, Option<u64>) {
        let TransportFrame::Single(RawJsonRpcMessage::Notification(
            notification,
        )) = frame
        else {
            panic!("expected one stream notification");
        };
        let Some(agent_client_protocol::RawJsonRpcParams::Object(params)) =
            notification.params.as_ref()
        else {
            panic!("expected named stream params");
        };
        let text = params
            .get("update")
            .and_then(|update| update.get("content"))
            .and_then(|content| content.get("text"))
            .and_then(serde_json::Value::as_str)
            .expect("stream text");
        let last_sequence = params
            .get("_meta")
            .and_then(|meta| meta.get("clawcode"))
            .and_then(|product| product.get("sessionRecovery"))
            .and_then(|recovery| recovery.get("lastSequence"))
            .and_then(serde_json::Value::as_u64);
        (text, last_sequence)
    }

    /// Runs the outbound relay with an explicit batch limit and negotiation state.
    async fn relay(
        frames: Vec<TransportFrame>,
        max_batch_size: usize,
        accepts: bool,
    ) -> Vec<TransportFrame> {
        let (source_tx, source_rx) = mpsc::unbounded();
        let (sink_tx, sink_rx) = mpsc::unbounded();
        for frame in frames {
            source_tx.unbounded_send(frame).expect("queue source frame");
        }
        drop(source_tx);

        OutboundRelay {
            max_batch_size: NonZeroUsize::new(max_batch_size)
                .expect("positive batch size"),
            accepts_batches: Arc::new(AtomicBool::new(accepts)),
        }
        .forward(source_rx, sink_tx)
        .await
        .expect("forward outbound frames");
        sink_rx.collect().await
    }

    /// Lists each single notification frame as its method and test index.
    fn single_indices(frames: &[TransportFrame]) -> Vec<(String, u64)> {
        frames
            .iter()
            .map(|frame| {
                let TransportFrame::Single(RawJsonRpcMessage::Notification(
                    notification,
                )) = frame
                else {
                    panic!("expected one single notification frame");
                };
                let index = notification
                    .params
                    .as_ref()
                    .and_then(|params| match params {
                        agent_client_protocol::RawJsonRpcParams::Object(
                            params,
                        ) => params.get("index"),
                        agent_client_protocol::RawJsonRpcParams::Array(_) => {
                            None
                        }
                    })
                    .and_then(serde_json::Value::as_u64)
                    .expect("test index");
                (notification.method.to_string(), index)
            })
            .collect()
    }

    /// Queued session updates leave the relay as ordered single frames.
    #[tokio::test]
    async fn queued_session_updates_stay_in_order() {
        let frames = relay(
            vec![
                notification("session/update", 1),
                notification("session/update", 2),
                notification("session/update", 3),
            ],
            8,
            false,
        )
        .await;

        assert_eq!(
            single_indices(&frames),
            vec![
                ("session/update".to_string(), 1),
                ("session/update".to_string(), 2),
                ("session/update".to_string(), 3),
            ]
        );
    }

    /// Queued chunks for one text block share one Session notification envelope.
    #[tokio::test]
    async fn forward_outbound_coalesces_queued_text_chunks() {
        let frames = relay(
            vec![stream_chunk(41, "Hel"), stream_chunk(42, "lo")],
            8,
            false,
        )
        .await;

        assert_eq!(frames.len(), 1);
        let TransportFrame::Single(RawJsonRpcMessage::Notification(
            notification,
        )) = &frames[0]
        else {
            panic!("coalesced chunks must produce one notification");
        };
        let text = notification
            .params
            .as_ref()
            .and_then(|params| match params {
                agent_client_protocol::RawJsonRpcParams::Object(params) => {
                    params.get("update")
                }
                agent_client_protocol::RawJsonRpcParams::Array(_) => None,
            })
            .and_then(|update| update.get("content"))
            .and_then(|content| content.get("text"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(text, Some("Hello"));
    }

    /// Coalescing never consumes more source chunks than one configured drain.
    #[tokio::test]
    async fn forward_outbound_limits_coalescing_to_batch_size() {
        let frames = relay(
            vec![
                stream_chunk(51, "Hel"),
                stream_chunk(52, "lo"),
                stream_chunk(53, "!"),
            ],
            2,
            false,
        )
        .await;

        assert_eq!(frames.len(), 2);
        assert_eq!(stream_chunk_data(&frames[0]), ("Hello", Some(52)));
        assert_eq!(stream_chunk_data(&frames[1]), ("!", None));
    }

    /// ToolCall updates stay ordered between two unmerged streaming notifications.
    #[tokio::test]
    async fn forward_outbound_keeps_tool_call_boundary() {
        let frames = relay(
            vec![
                stream_chunk(61, "before"),
                tool_call_update(),
                stream_chunk(62, "after"),
            ],
            8,
            false,
        )
        .await;

        assert_eq!(frames.len(), 3);
        assert!(matches!(
            &frames[1],
            TransportFrame::Single(RawJsonRpcMessage::Notification(notification))
                if notification.params.as_ref().is_some_and(|params| {
                    matches!(
                        params,
                        agent_client_protocol::RawJsonRpcParams::Object(params)
                            if params.get("update")
                                .and_then(|update| update.get("sessionUpdate"))
                                .and_then(serde_json::Value::as_str)
                                == Some("tool_call_update")
                    )
                })
        ));
    }

    /// Draining at the configured limit never drops or reorders updates.
    #[tokio::test]
    async fn drains_at_batch_limit_without_reordering() {
        let frames = relay(
            (0..7)
                .map(|index| notification("session/update", index))
                .collect(),
            3,
            false,
        )
        .await;

        assert_eq!(
            single_indices(&frames),
            (0..7)
                .map(|index| ("session/update".to_string(), index))
                .collect::<Vec<_>>()
        );
    }

    /// A non-Session frame keeps its position between queued updates.
    #[tokio::test]
    async fn preserves_order_across_non_session_frames() {
        let frames = relay(
            vec![
                notification("session/update", 1),
                notification("session/update", 2),
                notification("extension/changed", 3),
                notification("session/update", 4),
                notification("session/update", 5),
            ],
            8,
            false,
        )
        .await;

        assert_eq!(
            single_indices(&frames),
            vec![
                ("session/update".to_string(), 1),
                ("session/update".to_string(), 2),
                ("extension/changed".to_string(), 3),
                ("session/update".to_string(), 4),
                ("session/update".to_string(), 5),
            ]
        );
    }

    /// Inbound frames, including malformed ones, are relayed unchanged and in order.
    #[tokio::test]
    async fn inbound_frames_pass_through_unchanged() {
        let (source_tx, source_rx) = mpsc::unbounded();
        let (sink_tx, sink_rx) = mpsc::unbounded();
        source_tx
            .unbounded_send(TransportFrame::Malformed {
                raw: "[]".to_string(),
                error: agent_client_protocol::Error::invalid_request(),
            })
            .expect("queue malformed frame");
        source_tx
            .unbounded_send(notification("session/cancel", 1))
            .expect("queue inbound frame");
        drop(source_tx);

        InboundRelay {
            accepts_batches: Arc::new(AtomicBool::new(false)),
        }
        .forward(source_rx, sink_tx)
        .await
        .expect("forward inbound frames");

        let forwarded: Vec<TransportFrame> = sink_rx.collect().await;
        assert_eq!(forwarded.len(), 2);
        assert!(matches!(
            &forwarded[0],
            TransportFrame::Malformed { raw, .. } if raw == "[]"
        ));
        assert!(matches!(
            &forwarded[1],
            TransportFrame::Single(RawJsonRpcMessage::Notification(notification))
                if notification.method.as_ref() == "session/cancel"
        ));
    }

    /// Clients without the capability receive standard single frames.
    #[tokio::test]
    async fn unnegotiated_updates_stay_single_frames() {
        let frames =
            relay((1..=3).map(|index| update("s1", index)).collect(), 8, false)
                .await;

        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|frame| batch(frame).is_none()));
    }

    /// Queued same-Session updates share one carrier once negotiated.
    #[tokio::test]
    async fn negotiated_updates_share_one_carrier() {
        let frames =
            relay((1..=3).map(|index| update("s1", index)).collect(), 8, true)
                .await;

        assert_eq!(frames.len(), 1);
        assert_eq!(tool_ids(&frames[0]), ["tool-1", "tool-2", "tool-3"]);
        let nested = batch(&frames[0]).expect("carrier items");
        assert!(nested.iter().all(|item| item["sessionId"] == "s1"));
    }

    /// A carrier holds at most the configured number of updates in total.
    #[tokio::test]
    async fn carrier_respects_batch_limit() {
        let frames =
            relay((1..=5).map(|index| update("s1", index)).collect(), 2, true)
                .await;

        assert_eq!(frames.len(), 3);
        assert_eq!(tool_ids(&frames[0]), ["tool-1", "tool-2"]);
        assert_eq!(tool_ids(&frames[1]), ["tool-3", "tool-4"]);
        assert_eq!(tool_ids(&frames[2]), ["tool-5"]);
        assert!(batch(&frames[2]).is_none());
    }

    /// A batch limit of one never forms a carrier.
    #[tokio::test]
    async fn batch_limit_of_one_never_packs() {
        let frames =
            relay((1..=3).map(|index| update("s1", index)).collect(), 1, true)
                .await;

        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|frame| batch(frame).is_none()));
    }

    /// An update for another Session ends the current carrier in place.
    #[tokio::test]
    async fn session_change_ends_carrier() {
        let frames = relay(
            vec![
                update("s1", 1),
                update("s1", 2),
                update("s2", 3),
                update("s1", 4),
            ],
            8,
            true,
        )
        .await;

        assert_eq!(frames.len(), 3);
        assert_eq!(tool_ids(&frames[0]), ["tool-1", "tool-2"]);
        assert_eq!(tool_ids(&frames[1]), ["tool-3"]);
        assert_eq!(params(&frames[1])["sessionId"], "s2");
        assert_eq!(tool_ids(&frames[2]), ["tool-4"]);
        assert!(batch(&frames[2]).is_none());
    }

    /// A non-update frame ends the current carrier without reordering.
    #[tokio::test]
    async fn non_update_frame_ends_carrier() {
        let frames = relay(
            vec![
                update("s1", 1),
                update("s1", 2),
                notification("custom/event", 0),
                update("s1", 4),
            ],
            8,
            true,
        )
        .await;

        assert_eq!(frames.len(), 3);
        assert_eq!(tool_ids(&frames[0]), ["tool-1", "tool-2"]);
        assert!(matches!(
            &frames[1],
            TransportFrame::Single(RawJsonRpcMessage::Notification(notification))
                if notification.method.as_ref() == "custom/event"
        ));
        assert_eq!(tool_ids(&frames[2]), ["tool-4"]);
    }

    /// Chunks are coalesced before packing, so a merged run is one plain update.
    #[tokio::test]
    async fn coalescing_runs_before_packing() {
        let frames = relay(
            vec![stream_chunk(41, "Hel"), stream_chunk(42, "lo")],
            8,
            true,
        )
        .await;

        assert_eq!(frames.len(), 1);
        assert_eq!(stream_chunk_data(&frames[0]).0, "Hello");
        assert!(batch(&frames[0]).is_none());
    }

    /// Packing keeps every update's own product metadata.
    #[tokio::test]
    async fn carrier_preserves_existing_product_meta() {
        let frames = relay(
            (0..3)
                .map(|index| {
                    update_with_meta(
                        "s1",
                        index,
                        Some(serde_json::json!({
                            "clawcode": {
                                "sessionRecovery": { "projectionIndex": index }
                            }
                        })),
                    )
                })
                .collect(),
            8,
            true,
        )
        .await;

        assert_eq!(frames.len(), 1);
        let outer = params(&frames[0]);
        assert_eq!(
            outer["_meta"]["clawcode"]["sessionRecovery"]["projectionIndex"],
            0
        );
        let nested = batch(&frames[0]).expect("carrier items");
        let indices: Vec<_> = nested
            .iter()
            .map(|item| {
                item["_meta"]["clawcode"]["sessionRecovery"]["projectionIndex"]
                    .clone()
            })
            .collect();
        assert_eq!(indices, [1, 2]);
    }

    /// A non-object product namespace cannot hold the carrier key.
    #[tokio::test]
    async fn non_object_product_meta_falls_back_to_single_frames() {
        let frames = relay(
            vec![
                update_with_meta(
                    "s1",
                    1,
                    Some(serde_json::json!({ "clawcode": "x" })),
                ),
                update("s1", 2),
            ],
            8,
            true,
        )
        .await;

        assert_eq!(frames.len(), 2);
        assert_eq!(params(&frames[0])["_meta"]["clawcode"], "x");
        assert!(batch(&frames[1]).is_none());
    }

    /// Every `initialize` request re-evaluates the batching capability.
    #[test]
    fn initialize_meta_toggles_batching() {
        let relay = InboundRelay {
            accepts_batches: Arc::new(AtomicBool::new(false)),
        };
        let accepts = || relay.accepts_batches.load(Ordering::Acquire);

        relay.observe(&initialize(Some(batch_capability())));
        assert!(accepts());
        relay.observe(&notification("session/cancel", 1));
        assert!(accepts());
        relay.observe(&initialize(None));
        assert!(!accepts());

        relay.observe(&initialize(Some(batch_capability())));
        relay.observe(&initialize(Some(serde_json::json!({
            "clawcode": { "sessionUpdateBatch": { "version": 2 } }
        }))));
        assert!(!accepts());

        relay.observe(&initialize(Some(batch_capability())));
        relay.observe(&initialize(Some(serde_json::json!({
            "clawcode": { "sessionUpdateBatch": "yes" }
        }))));
        assert!(!accepts());
    }

    /// The wrapped component packs updates only after a negotiated `initialize`.
    #[tokio::test]
    async fn component_batches_only_after_negotiated_initialize() {
        for (meta, expected_frames) in
            [(Some(batch_capability()), 1), (None, 3)]
        {
            let (inner, agent) = Channel::duplex();
            let (outer, driver) = <BatchComponent<Channel> as ConnectTo<
                Client,
            >>::into_channel_and_future(
                BatchComponent::new(
                    inner,
                    NonZeroUsize::new(8).expect("positive batch size"),
                ),
            );
            let driver = tokio::spawn(driver.expect("relay driver"));
            let (mut client_rx, client_tx) = outer.into_parts();
            let (mut agent_rx, agent_tx) = agent.into_parts();

            client_tx
                .unbounded_send(initialize(meta))
                .expect("send initialize");
            let received = agent_rx.next().await.expect("forwarded initialize");
            assert!(matches!(
                received,
                TransportFrame::Single(RawJsonRpcMessage::Request(request))
                    if request.method.as_ref() == "initialize"
            ));
            for index in 1..=3 {
                agent_tx
                    .unbounded_send(update("s1", index))
                    .expect("queue update");
            }

            let mut frames = Vec::new();
            for _ in 0..expected_frames {
                frames.push(client_rx.next().await.expect("outbound frame"));
            }
            let ids: Vec<String> = frames.iter().flat_map(tool_ids).collect();
            assert_eq!(ids, ["tool-1", "tool-2", "tool-3"]);
            assert!(client_rx.next().now_or_never().is_none());
            driver.abort();
        }
    }
}
