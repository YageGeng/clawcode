use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{mpsc, watch};
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::network::SharedEndpoint;

const HANDSHAKE_LIMIT: usize = 8192;
const FRAME_LIMIT: usize = 32 * 1024 * 1024;
pub(crate) const DENIED: u32 = 1;
pub(crate) const REVOKED: u32 = 2;
pub(crate) const STOPPED: u32 = 3;
pub(crate) const DISCONNECTED: u32 = 4;
pub(crate) const INCOMPATIBLE: &str =
    "Incompatible protocol version; upgrade both devices";

/// A cloneable connection handle. Closing one handle wakes both frame pumps.
#[derive(Clone)]
pub struct SessionConnection {
    pub(crate) connection: Connection,
    status: watch::Receiver<Option<&'static str>>,
}

impl SessionConnection {
    /// Wake as soon as QUIC closes so the application can cancel in-flight ACP work.
    pub async fn wire_closed(&self) {
        self.connection.closed().await;
    }

    /// Close the authenticated connection immediately.
    pub fn close(&self) {
        // noq records LocallyClosed on every explicit close, even after receiving
        // a peer close. Preserve the original peer code for late UI observers.
        if self.connection.close_reason().is_none() {
            self.connection.close(DISCONNECTED.into(), b"");
        }
    }

    /// Wait for closure and return only locally defined status strings to the UI.
    pub async fn closed(&self) -> String {
        let mut status = self.status.clone();
        status
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|reason| *reason)
            .unwrap_or("Remote connection closed")
            .to_owned()
    }

    /// Translate close codes without reflecting peer-controlled text into logs or the UI.
    pub(crate) fn reason(error: ConnectionError) -> &'static str {
        match error {
            ConnectionError::ApplicationClosed(close) => {
                match close.error_code.into_inner() {
                    1 => "Pairing or device access denied",
                    2 => "Device access revoked",
                    3 => "Remote hosting stopped",
                    4 => "Remote device disconnected",
                    _ => "Remote connection closed",
                }
            }
            ConnectionError::TimedOut => "Remote connection timed out",
            ConnectionError::VersionMismatch => INCOMPATIBLE,
            _ => "Remote connection closed",
        }
    }
}

/// Handshake and ACP share one ordered bidirectional QUIC stream.
pub(crate) struct Wire {
    receive: FramedRead<RecvStream, LinesCodec>,
    send: SendStream,
}

impl From<(SendStream, RecvStream)> for Wire {
    /// Preserve buffered bytes when switching from bounded handshake messages to ACP.
    fn from((send, receive): (SendStream, RecvStream)) -> Self {
        Self {
            receive: FramedRead::new(
                receive,
                LinesCodec::new_with_max_length(HANDSHAKE_LIMIT),
            ),
            send,
        }
    }
}

impl Wire {
    /// Read one length-limited JSON handshake message without reporting the peer's payload.
    pub(crate) async fn read<T: DeserializeOwned>(&mut self) -> Result<T> {
        let line = self
            .receive
            .next()
            .await
            .context("Remote handshake ended")?
            .map_err(|_peer_input_error| {
                anyhow::anyhow!("Invalid handshake frame")
            })?;
        serde_json::from_str(&line).map_err(|_peer_input_error| {
            anyhow::anyhow!("Invalid handshake message")
        })
    }

    /// Write a complete protocol handshake message on its own line.
    pub(crate) async fn write<T: Serialize>(
        &mut self,
        message: &T,
    ) -> Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        ensure!(
            bytes.len() <= HANDSHAKE_LIMIT,
            "Handshake exceeds size limit"
        );
        bytes.push(b'\n');
        self.send
            .write_all(&bytes)
            .await
            .context("Could not send handshake")?;
        Ok(())
    }

    /// Pump ACP asynchronously in both directions, canceling backpressure waits on disconnect.
    pub(crate) fn session(
        self,
        outbound: Option<Arc<SharedEndpoint>>,
        connection: Connection,
    ) -> RemoteSession {
        let (finished, status) = watch::channel(None);
        let control = SessionConnection { connection, status };
        let (deliver, incoming) = mpsc::channel(8);
        let (outgoing, mut send_queue) = mpsc::channel::<String>(8);
        let reader = self
            .receive
            .map_decoder(|_| LinesCodec::new_with_max_length(FRAME_LIMIT));
        let pump = control.clone();
        tokio::spawn(async move {
            let read = async {
                let mut reader = reader;
                while let Some(Ok(line)) = reader.next().await {
                    if deliver.send(line).await.is_err() {
                        break;
                    }
                }
            };
            let write = async move {
                let mut send = self.send;
                while let Some(line) = send_queue.recv().await {
                    // A frame carries one ACP JSON text; embedded newlines in values are JSON escapes.
                    if line.len() > FRAME_LIMIT || line.contains(['\r', '\n']) {
                        tracing::warn!("Rejected invalid outbound ACP line");
                        break;
                    }
                    if send.write_all(line.as_bytes()).await.is_err()
                        || send.write_all(b"\n").await.is_err()
                    {
                        break;
                    }
                }
            };
            tokio::select! {
                biased;
                _ = pump.connection.closed() => {}
                // An idle socket will never attempt a send that notices the frame reader was disposed.
                () = deliver.closed() => pump.close(),
                _ = read => pump.close(),
                _ = write => pump.close(),
            }
            // Endpoint::close marks QUIC connections locally closed. Capture
            // the authenticated peer's status before the last role drains iroh.
            let reason =
                SessionConnection::reason(pump.connection.closed().await);
            // The listener owns inbound endpoint lifetime. An outbound session
            // holds its role only until its actual connection ends.
            if let Some(network) = outbound {
                network.release().await;
            }
            finished.send_replace(Some(reason));
        });
        RemoteSession {
            incoming,
            outgoing,
            control,
        }
    }
}

/// Bounded, bidirectional ACP frame channels for one authenticated connection.
pub struct RemoteSession {
    pub incoming: mpsc::Receiver<String>,
    pub outgoing: mpsc::Sender<String>,
    pub(crate) control: SessionConnection,
}

impl RemoteSession {
    /// Close the session and wake pending reads.
    pub fn close(&self) {
        self.control.close();
    }

    /// Wait for the peer or local owner to end this session.
    pub async fn closed(&self) -> String {
        self.control.closed().await
    }

    /// Give the native bridge independent ownership of each frame direction and close control.
    pub fn into_parts(
        self,
    ) -> (
        mpsc::Receiver<String>,
        mpsc::Sender<String>,
        SessionConnection,
    ) {
        (self.incoming, self.outgoing, self.control)
    }
}
