//! An outgoing ACP session. The controller never needs a local kernel or enabled listener.

use std::sync::{Arc, OnceLock};

use anyhow::{Result, bail};
use nsh_core::prelude::*;
use remote::{
    NetworkConfig, Peer, PendingPairing, RemoteClient, RemoteSession,
    SessionConnection,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

use crate::hosting::{HostingState, RemoteDevice, RemoteHosting};
use crate::runtime::NETWORK;

const CONGESTED: &str =
    "Remote connection closed because its outgoing queue was full";
const FRAME_DELIVERY_FAILED: &str = "Could not deliver remote ACP frames";
const BATCH_FRAMES: usize = 8;
const BATCH_BYTES: usize = 64 * 1024;

/// Groups ready frames for the engine without waiting to fill a batch.
struct IncomingFrames(mpsc::Receiver<String>);

impl IncomingFrames {
    /// Waits for each consumer acknowledgement before taking another batch from the wire.
    async fn deliver<F: Future<Output = Result<(), String>>>(
        &mut self,
        mut consume: impl FnMut(Vec<String>) -> F,
    ) -> Result<(), String> {
        let mut next = self.0.recv().await;
        while let Some(first) = next.take() {
            let mut bytes = first.len();
            let mut batch = vec![first];
            while batch.len() < BATCH_FRAMES && bytes < BATCH_BYTES {
                let Ok(frame) = self.0.try_recv() else { break };
                if bytes + frame.len() > BATCH_BYTES {
                    // Leave this frame intact for the next batch; a large frame travels alone.
                    next = Some(frame);
                    break;
                }
                bytes += frame.len();
                batch.push(frame);
            }
            consume(batch).await?;
            if next.is_none() {
                next = self.0.recv().await;
            }
        }
        Ok(())
    }
}

/// A live authenticated connection to another desktop's ACP server.
#[class]
#[derive(TypedBuilder)]
pub struct RemoteConnection {
    peer: RemoteDevice,
    #[builder(default, setter(strip_option))]
    incoming: Option<mpsc::Receiver<String>>,
    outgoing: mpsc::Sender<String>,
    connection: SessionConnection,
    #[builder(default)]
    local_end: Arc<OnceLock<Option<&'static str>>>,
}

impl HostingState {
    /// Close one prepared or adopted outgoing connection when its root closes, independently of the kernel.
    fn watch_connection(&self, connection: &SessionConnection) {
        let root_closed = self.closed.clone();
        let cleanup = connection.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = root_closed.cancelled() => cleanup.close(),
                _ = cleanup.wire_closed() => {}
            }
        });
    }
}

impl From<(Peer, RemoteSession)> for RemoteConnection {
    /// Take ownership of both frame directions after admission has been committed.
    fn from((peer, session): (Peer, RemoteSession)) -> Self {
        let (incoming, outgoing, connection) = session.into_parts();
        Self::builder()
            .peer(peer.into())
            .incoming(incoming)
            .outgoing(outgoing)
            .connection(connection)
            .build()
    }
}

#[class]
impl RemoteConnection {
    /// Reconnects to a previously paired host, verifying the peer's saved public identity.
    pub async fn connect(
        hosting: Instance<RemoteHosting>,
        host_id: String,
        relay: Option<String>,
    ) -> Result<RemoteConnection, String> {
        let state = hosting.with(RemoteHosting::state);
        NETWORK.start("remote connection", async move {
            let config = NetworkConfig::from_relay_url(relay.as_deref())?;
            let connected = tokio::select! {
                biased;
                () = state.closed.cancelled() => bail!("remote controls are closed"),
                result = RemoteClient::connect(&state.profile, config, &host_id) => result?,
            };
            let remote = Self::from(connected);
            state.watch_connection(&remote.connection);
            state.changes.send_replace(());
            Ok(remote)
        }).await
    }

    /// The remote host's authenticated identity and display name.
    #[getter]
    pub fn peer(&self) -> RemoteDevice {
        self.peer.clone()
    }

    /// Queues a single outbound ACP frame in order, failing explicitly on congestion or closure.
    pub fn send(&self, frame: String) -> Result<(), String> {
        if let Some(reason) = self.local_end.get() {
            return Err(reason.unwrap_or("Remote connection closed").into());
        }
        self.outgoing.try_send(frame).map_err(|error| {
            // Never format a failed send with its frame; it may hold workspace or credential data.
            let reason = match error {
                mpsc::error::TrySendError::Full(_) => {
                    // A refused frame could be a permission reply with no request promise to retry.
                    // End this authenticated link so both sides release all outstanding responders.
                    let _ = self.local_end.set(Some(CONGESTED));
                    self.connection.close();
                    CONGESTED
                }
                mpsc::error::TrySendError::Closed(_) => {
                    "the remote connection is closed"
                }
            };
            tracing::warn!("desktop ACP send failed: {}", reason);
            reason.to_owned()
        })
    }

    /// Delivers ordered batches, waiting for each listener call, then returns the close reason or null for local close.
    pub async fn serve(
        this: Instance<Self>,
        #[args(frames)] listener: Callback<Vec<String>>,
    ) -> Result<Option<String>, String> {
        let (mut incoming, connection, local_end) = this
            .with_mut(|session| {
                session.incoming.take().map(|incoming| {
                    (
                        IncomingFrames(incoming),
                        session.connection.clone(),
                        Arc::clone(&session.local_end),
                    )
                })
            })
            .ok_or_else(|| {
                tracing::warn!(
                    "desktop remote ACP frames already have a listener"
                );
                "the remote connection's frames are already served".to_owned()
            })?;
        // A revoked link or closed window must release the reader even if JS never acknowledges.
        tokio::select! {
            biased;
            () = connection.wire_closed() => {}
            result = incoming.deliver(|batch| listener.call_async(batch)) => {
                if result.is_err() {
                    // Callback errors can include frame content. Expose only a local diagnostic.
                    tracing::warn!("desktop remote ACP frame listener failed");
                    let _ = local_end.set(Some(FRAME_DELIVERY_FAILED));
                    connection.close();
                }
            }
        }
        drop(incoming);
        let reason = connection.closed().await;
        Ok(match local_end.get() {
            Some(local) => local.map(str::to_owned),
            None => Some(reason),
        })
    }

    /// Immediately closes this authenticated connection and releases its transport.
    pub fn close(&self) {
        if self.local_end.set(None).is_ok() {
            self.connection.close();
        }
    }
}

/// One outgoing dialog attempt, with an explicit point at which the UI adopts the peer.
#[class]
#[derive(TypedBuilder)]
pub struct RemotePairing {
    state: Arc<HostingState>,
    cancelled: CancellationToken,
    #[builder(default)]
    started: bool,
    #[builder(default)]
    prepared: Option<PendingPairing>,
}

impl From<Arc<HostingState>> for RemotePairing {
    /// Allocate a cancellation scope before the first asynchronous network operation starts.
    fn from(state: Arc<HostingState>) -> Self {
        Self::builder()
            .cancelled(state.closed.child_token())
            .state(state)
            .build()
    }
}

#[class]
impl RemotePairing {
    /// Wait for host approval without saving a host or making the transport available to the UI.
    pub async fn dial(
        this: Instance<Self>,
        code: String,
        relay: Option<String>,
    ) -> Result<(), String> {
        let (state, cancelled) = this.with_mut(|attempt| {
            if std::mem::replace(&mut attempt.started, true) {
                tracing::warn!("desktop pairing attempt was already started");
                return Err("Pairing attempt already started".to_owned());
            }
            Ok((Arc::clone(&attempt.state), attempt.cancelled.clone()))
        })?;
        let prepared = NETWORK.start("pairing attempt", async move {
            let config = NetworkConfig::from_relay_url(relay.as_deref())?;
            let prepared = tokio::select! {
                biased;
                () = cancelled.cancelled() => bail!("Pairing cancelled"),
                result = RemoteClient::prepare_pair(&state.profile, config, &code) => result?,
            };
            state.watch_connection(prepared.connection());
            Ok(prepared)
        }).await?;
        this.with_mut(|attempt| {
            // The dialog can dismiss between an approved network result and its engine continuation.
            if attempt.cancelled.is_cancelled() {
                tracing::debug!("discarded a cancelled desktop pairing result");
                return Err("Pairing cancelled".into());
            }
            attempt.prepared = Some(prepared);
            Ok(())
        })
    }

    /// Commit the approved host and return its connection in the same synchronous UI action.
    pub fn accept(&mut self) -> Result<RemoteConnection, String> {
        if self.cancelled.is_cancelled() {
            tracing::debug!(
                "skipped adoption of a cancelled desktop pairing attempt"
            );
            return Err("Pairing cancelled".into());
        }
        let prepared = self.prepared.take().ok_or_else(|| {
            tracing::warn!(
                "desktop pairing attempt has no approved connection"
            );
            "Pairing has not completed".to_owned()
        })?;
        let (peer, session) = prepared.accept().map_err(|error| {
            tracing::warn!(
                "failed to save approved desktop pairing: {:#}",
                error
            );
            format!("{error:#}")
        })?;
        self.state.changes.send_replace(());
        Ok(RemoteConnection::from((peer, session)))
    }

    /// Abort an outstanding handshake or discard an approved but unadopted connection.
    pub fn cancel(&mut self) {
        self.cancelled.cancel();
        self.prepared.take();
    }
}

impl Drop for RemotePairing {
    /// Dispose this dialog's wire if the native attempt is released without adoption.
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Drop for RemoteConnection {
    /// Closes the wire when the last NSH owner disposes the connection.
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use futures::poll;
    use tokio::sync::{mpsc, oneshot};

    use super::{BATCH_BYTES, BATCH_FRAMES, IncomingFrames};

    /// A paused consumer keeps the wire queue bounded and receives whole frames in order after each ack.
    #[tokio::test]
    async fn frames_wait_for_consumer_acknowledgement() {
        let (writer, receiver) = mpsc::channel(BATCH_FRAMES + 2);
        let mut frames = IncomingFrames(receiver);
        let (calls, mut consumer) = mpsc::channel(1);
        let mut delivery = Box::pin(frames.deliver(|batch| {
            let (ack, reply) = oneshot::channel();
            calls
                .try_send((batch, ack))
                .expect("one outstanding callback");
            async { reply.await.expect("consumer acknowledged") }
        }));

        writer.try_send("first".to_owned()).expect("initial frame");
        assert!(poll!(&mut delivery).is_pending());
        let (batch, first) = consumer.try_recv().expect("initial callback");
        assert_eq!(batch, ["first"]);
        let expected: Vec<_> =
            (0..BATCH_FRAMES).map(|n| n.to_string()).collect();
        let large = "x".repeat(BATCH_BYTES + 1);
        for frame in &expected {
            writer.try_send(frame.clone()).expect("small frame");
        }
        writer.try_send("tail".into()).expect("last small frame");
        writer.try_send(large.clone()).expect("large frame");
        assert!(poll!(&mut delivery).is_pending());
        assert_eq!(writer.capacity(), 0);
        assert!(matches!(
            consumer.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        first.send(Ok(())).expect("first ack");
        assert!(poll!(&mut delivery).is_pending());
        let (batch, second) = consumer.try_recv().expect("small batch");
        assert_eq!(batch, expected);
        second.send(Ok(())).expect("second ack");
        assert!(poll!(&mut delivery).is_pending());
        let (batch, third) = consumer.try_recv().expect("tail");
        assert_eq!(batch, ["tail"]);
        third.send(Ok(())).expect("third ack");
        assert!(poll!(&mut delivery).is_pending());
        let (batch, last) = consumer.try_recv().expect("large frame");
        assert_eq!(batch, [large]);

        // A failing JS listener ends delivery before it can consume any more frames.
        writer.try_send("after error".into()).expect("queued frame");
        last.send(Err("listener failed".into()))
            .expect("reject callback");
        assert_eq!(delivery.await, Err("listener failed".to_owned()));
        assert!(matches!(
            consumer.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}
