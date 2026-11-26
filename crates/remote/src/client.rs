use std::time::Duration;

use anyhow::{Context, Result, ensure};
use iroh::{
    EndpointAddr,
    endpoint::{ConnectError, ConnectingError, Connection, ConnectionError},
};
use tokio::time::timeout;
use typed_builder::TypedBuilder;

use crate::{
    DeviceProfile, NetworkConfig, Peer, RemoteSession,
    pairing::{ALPN, Hello, Ticket},
    profile::KnownHost,
    session::{DISCONNECTED, INCOMPATIBLE, SessionConnection, Wire},
};

/// Explicitly close just this QUIC connection if the owning dial or admission is discarded.
struct PendingHandshake {
    connection: Connection,
    accepted: bool,
}

impl Drop for PendingHandshake {
    /// Dropping an unadopted dial also withdraws a pending prompt on the host.
    fn drop(&mut self) {
        if !self.accepted && self.connection.close_reason().is_none() {
            self.connection.close(DISCONNECTED.into(), b"");
        }
    }
}

/// An approved connection awaiting the controller's synchronous decision to save and use it.
#[derive(TypedBuilder)]
pub struct PendingPairing {
    profile: DeviceProfile,
    host: KnownHost,
    peer: Peer,
    session: RemoteSession,
    handshake: PendingHandshake,
}

impl PendingPairing {
    /// Observe and close the authenticated link while the controller decides whether to adopt it.
    pub fn connection(&self) -> &SessionConnection {
        &self.session.control
    }

    /// Save the authenticated address and transfer ownership to the live session.
    pub fn accept(mut self) -> Result<(Peer, RemoteSession)> {
        if let Some(reason) = self.handshake.connection.close_reason() {
            let reason = SessionConnection::reason(reason);
            tracing::warn!(
                "Cannot adopt a closed remote connection: {}",
                reason
            );
            anyhow::bail!(reason);
        }
        self.profile.remember(self.host)?;
        self.handshake.accepted = true;
        tracing::info!(
            "Connected to remote host {}",
            self.handshake.connection.remote_id().fmt_short()
        );
        Ok((self.peer, self.session))
    }
}

/// A controller dials the authenticated host identity in its invitation or address book.
pub struct RemoteClient<'a> {
    profile: &'a DeviceProfile,
    config: NetworkConfig,
}

impl<'a> RemoteClient<'a> {
    /// Submit an invitation and wait for explicit approval on the remote desktop.
    pub async fn pair(
        profile: &'a DeviceProfile,
        config: NetworkConfig,
        code: &str,
    ) -> Result<(Peer, RemoteSession)> {
        Self::prepare_pair(profile, config, code).await?.accept()
    }

    /// Authenticate without saving the host until the controller adopts the prepared session.
    pub async fn prepare_pair(
        profile: &'a DeviceProfile,
        config: NetworkConfig,
        code: &str,
    ) -> Result<PendingPairing> {
        let ticket = Ticket::parse(code)?;
        let hello = Hello::Pair {
            secret: ticket.secret,
            name: profile.name(),
        };
        Self { profile, config }.dial(ticket.addr, hello).await
    }

    /// Resume an already paired host using its saved identity and relay hints.
    pub async fn connect(
        profile: &'a DeviceProfile,
        config: NetworkConfig,
        host_id: &str,
    ) -> Result<(Peer, RemoteSession)> {
        let saved = profile.host(host_id)?;
        Self { profile, config }
            .dial(saved.addr, Hello::Connect)
            .await?
            .accept()
    }

    /// Authenticate the host with QUIC, complete one bounded handshake, then expose frame channels.
    async fn dial(
        self,
        addr: EndpointAddr,
        hello: Hello,
    ) -> Result<PendingPairing> {
        let id = addr.id;
        let endpoint = self.profile.endpoint(self.config, false).await?;
        ensure!(
            id != endpoint.endpoint.id(),
            "Cannot connect to this device's own profile"
        );
        tracing::info!("Connecting to remote host {}", id.fmt_short());
        let conn = timeout(
            Duration::from_secs(20),
            endpoint.endpoint.connect(addr.clone(), ALPN),
        )
        .await
        .context("Remote host connection timed out")?
        .map_err(|err| {
            tracing::warn!(
                "Could not connect to remote host {}",
                id.fmt_short()
            );
            // TLS alert 120 (QUIC 0x178) means no application protocol was negotiated.
            let incompatible = match &err {
                ConnectError::Connection { source, .. }
                | ConnectError::Connecting {
                    source: ConnectingError::ConnectionError { source, .. },
                    ..
                } => match source {
                    ConnectionError::VersionMismatch => true,
                    ConnectionError::ConnectionClosed(close) => {
                        u64::from(close.error_code) == 0x178
                    }
                    ConnectionError::TransportError(error) => {
                        u64::from(error.code) == 0x178
                    }
                    _ => false,
                },
                _ => false,
            };
            if incompatible {
                anyhow::anyhow!(INCOMPATIBLE)
            } else {
                anyhow::anyhow!("Remote host is unavailable")
            }
        })?;
        let handshake = PendingHandshake {
            connection: conn,
            accepted: false,
        };
        let conn = &handshake.connection;
        let mut wire = Wire::from(
            conn.open_bi()
                .await
                .context("Could not open the remote handshake stream")?,
        );
        wire.write(&hello).await?;
        let welcome =
            timeout(Duration::from_secs(100), wire.read::<KnownHost>())
                .await
                .context("Host approval timed out")?;
        let welcome = welcome.map_err(|error| {
            tracing::warn!(
                "Handshake with remote host {} ended before approval",
                id.fmt_short()
            );
            conn.close_reason()
                .map(|reason| {
                    anyhow::anyhow!(SessionConnection::reason(reason))
                })
                .unwrap_or(error)
        })?;
        ensure!(welcome.addr.id == id, "Host identity mismatch");
        let peer = Peer::new(id, &welcome.name)?;
        // Prefer the current addresses. Reuse the old hints only for a missing
        // transport so reconnecting on different networks does not grow the address book.
        let mut current = welcome.addr;
        if current.relay_urls().next().is_none() {
            current = current.with_addrs(
                addr.relay_urls().cloned().map(iroh::TransportAddr::Relay),
            );
        }
        if current.ip_addrs().next().is_none() {
            current = current.with_addrs(
                addr.ip_addrs().copied().map(iroh::TransportAddr::Ip),
            );
        }
        let saved = KnownHost {
            addr: current,
            name: peer.name.clone(),
        };
        Ok(PendingPairing::builder()
            .profile(self.profile.clone())
            .host(saved)
            .peer(peer)
            .session(wire.session(Some(endpoint), conn.clone()))
            .handshake(handshake)
            .build())
    }
}
