use std::num::NonZeroUsize;

use agent_client_protocol::{
    Agent, Channel, Client, ConnectTo, RawJsonRpcMessage, TransportBatch,
    TransportFrame,
};
use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::{FutureExt as _, StreamExt as _};

use crate::coalescing::coalesce_updates;

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

/// Returns whether one raw message is a standard ACP Session update notification.
fn is_session_update(message: &RawJsonRpcMessage) -> bool {
    matches!(
        message,
        RawJsonRpcMessage::Notification(notification)
            if notification.method.as_ref() == "session/update"
    )
}

/// Forwards outbound frames while batching only currently queued Session updates.
async fn forward_outbound(
    mut source: mpsc::UnboundedReceiver<TransportFrame>,
    sink: mpsc::UnboundedSender<TransportFrame>,
    max_batch_size: NonZeroUsize,
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
            sink.unbounded_send(frame)
                .map_err(agent_client_protocol::Error::into_internal_error)?;
            continue;
        };
        if !is_session_update(&message) {
            sink.unbounded_send(TransportFrame::Single(message))
                .map_err(agent_client_protocol::Error::into_internal_error)?;
            continue;
        }

        let mut messages = vec![message];
        let mut source_closed = false;
        // Stop draining at the configured limit so one replay cannot create an
        // unbounded JSON-RPC frame and monopolize browser-side rendering.
        while messages.len() < max_batch_size.get() {
            match source.next().now_or_never() {
                Some(Some(TransportFrame::Single(message)))
                    if is_session_update(&message) =>
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
        // Coalesce only the messages already drained for this frame so the
        // optimization cannot add a timer or delay the first streaming token.
        let mut messages = coalesce_updates(messages);
        let frame = if messages.len() == 1 {
            TransportFrame::Single(
                messages.pop().expect("one queued Session update"),
            )
        } else {
            TransportFrame::Batch(
                TransportBatch::from_messages(messages)
                    .expect("non-empty queued Session updates"),
            )
        };
        sink.unbounded_send(frame)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        if source_closed {
            return Ok(());
        }
    }
}

/// Forwards inbound transport frames without rewriting their boundaries.
async fn forward_inbound(
    mut source: mpsc::UnboundedReceiver<TransportFrame>,
    sink: mpsc::UnboundedSender<TransportFrame>,
) -> Result<(), agent_client_protocol::Error> {
    while let Some(frame) = source.next().await {
        sink.unbounded_send(frame)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
    }
    Ok(())
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
        let (channel, component_future) =
            <Self as ConnectTo<Client>>::into_channel_and_future(self);
        let client_future =
            <Channel as ConnectTo<Client>>::connect_to(channel, client);
        futures::try_join!(component_future, client_future)?;
        Ok(())
    }

    /// Exposes a frame-aware channel and drives both relay directions to completion.
    fn into_channel_and_future(
        self,
    ) -> (
        Channel,
        BoxFuture<'static, Result<(), agent_client_protocol::Error>>,
    ) {
        let (inner_channel, inner_future) =
            self.inner.into_channel_and_future();
        let (outer_channel, relay_channel) = Channel::duplex();
        let Channel {
            rx: inner_rx,
            tx: inner_tx,
        } = inner_channel;
        let Channel {
            rx: outer_rx,
            tx: outer_tx,
        } = relay_channel;
        let max_batch_size = self.max_batch_size;
        let relay = Box::pin(async move {
            futures::try_join!(
                inner_future,
                forward_inbound(outer_rx, inner_tx),
                forward_outbound(inner_rx, outer_tx, max_batch_size),
            )?;
            Ok(())
        });
        (outer_channel, relay)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use agent_client_protocol::{RawJsonRpcMessage, TransportFrame};
    use futures::StreamExt as _;
    use futures::channel::mpsc;

    use super::{forward_inbound, forward_outbound};

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

    /// Runs the outbound relay with an explicit configured batch limit.
    async fn relay(
        frames: Vec<TransportFrame>,
        max_batch_size: usize,
    ) -> Vec<TransportFrame> {
        let (source_tx, source_rx) = mpsc::unbounded();
        let (sink_tx, sink_rx) = mpsc::unbounded();
        for frame in frames {
            source_tx.unbounded_send(frame).expect("queue source frame");
        }
        drop(source_tx);

        forward_outbound(
            source_rx,
            sink_tx,
            NonZeroUsize::new(max_batch_size).expect("positive batch size"),
        )
        .await
        .expect("forward outbound frames");
        sink_rx.collect().await
    }

    /// Consecutive queued session updates share one standard JSON-RPC batch.
    #[tokio::test]
    async fn batches_queued_session_updates() {
        let frames = relay(
            vec![
                notification("session/update", 1),
                notification("session/update", 2),
                notification("session/update", 3),
            ],
            8,
        )
        .await;

        assert_eq!(frames.len(), 1);
        let TransportFrame::Batch(batch) = &frames[0] else {
            panic!("three queued updates must produce one batch");
        };
        assert_eq!(batch.len(), 3);
    }

    /// Queued chunks for one text block share one Session notification envelope.
    #[tokio::test]
    async fn forward_outbound_coalesces_queued_text_chunks() {
        let frames =
            relay(vec![stream_chunk(41, "Hel"), stream_chunk(42, "lo")], 8)
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
        )
        .await;

        assert_eq!(frames.len(), 1);
        let TransportFrame::Batch(batch) = &frames[0] else {
            panic!("three boundary updates must remain one ordered batch");
        };
        assert_eq!(batch.len(), 3);
        let middle = batch.entries().nth(1).expect("middle batch entry");
        assert!(matches!(
            middle,
            agent_client_protocol::TransportBatchEntry::Message(
                RawJsonRpcMessage::Notification(notification)
            ) if notification.params.as_ref().is_some_and(|params| {
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

    /// One transport batch never exceeds the configured bounded size.
    #[tokio::test]
    async fn splits_session_updates_at_batch_limit() {
        let frames = relay(
            (0..7)
                .map(|index| notification("session/update", index))
                .collect(),
            3,
        )
        .await;

        assert_eq!(frames.len(), 3);
        assert!(
            matches!(&frames[0], TransportFrame::Batch(batch) if batch.len() == 3)
        );
        assert!(
            matches!(&frames[1], TransportFrame::Batch(batch) if batch.len() == 3)
        );
        assert!(matches!(&frames[2], TransportFrame::Single(_)));
    }

    /// A non-Session frame flushes queued updates without changing source order.
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
        )
        .await;

        assert_eq!(frames.len(), 3);
        assert!(
            matches!(&frames[0], TransportFrame::Batch(batch) if batch.len() == 2)
        );
        assert!(matches!(
            &frames[1],
            TransportFrame::Single(RawJsonRpcMessage::Notification(notification))
                if notification.method.as_ref() == "extension/changed"
        ));
        assert!(
            matches!(&frames[2], TransportFrame::Batch(batch) if batch.len() == 2)
        );
    }

    /// Inbound transport frames are relayed without changing batch boundaries.
    #[tokio::test]
    async fn leaves_inbound_frames_unchanged() {
        let (source_tx, source_rx) = mpsc::unbounded();
        let (sink_tx, mut sink_rx) = mpsc::unbounded();
        let frame = notification("session/cancel", 1);
        source_tx
            .unbounded_send(frame.clone())
            .expect("queue inbound frame");
        drop(source_tx);

        forward_inbound(source_rx, sink_tx)
            .await
            .expect("forward inbound frame");

        let forwarded = sink_rx.next().await.expect("forwarded frame");
        assert!(matches!(
            forwarded,
            TransportFrame::Single(RawJsonRpcMessage::Notification(notification))
                if notification.method.as_ref() == "session/cancel"
        ));
        assert!(sink_rx.next().await.is_none());
    }
}
