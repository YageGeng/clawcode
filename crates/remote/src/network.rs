use crate::DeviceProfile;
use anyhow::{Context, Result, ensure};
use iroh::{
    Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey, TransportAddr,
    endpoint::{default_relay_mode, presets},
};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::{
    sync::oneshot,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

/// Choose the public development network, a configured relay, or isolated loopback.
#[derive(Default, Clone, PartialEq, Eq)]
pub enum NetworkConfig {
    #[default]
    Public,
    Relay(RelayUrl),
    Loopback,
}

#[derive(TypedBuilder)]
pub(crate) struct SharedEndpoint {
    _lifetime: oneshot::Sender<()>,
    pub(crate) drained: CancellationToken,
    pub(crate) endpoint: Endpoint,
    pub(crate) hosting: AtomicBool,
    pub(crate) config: NetworkConfig,
}

impl SharedEndpoint {
    /// Supply the active relay when reachable and fall back to local addresses on isolated LANs.
    pub(crate) async fn address(&self) -> EndpointAddr {
        let ep = &self.endpoint;
        if self.config != NetworkConfig::Loopback
            && ep.addr().relay_urls().next().is_none()
        {
            let _ = timeout(Duration::from_secs(3), ep.online()).await;
        }
        let mut addr = ep.addr();
        if self.config == NetworkConfig::Loopback {
            addr = addr.with_addrs(
                ep.bound_sockets().into_iter().map(iroh::TransportAddr::Ip),
            );
        }
        self.config.contact_hints(addr)
    }

    /// Drop one live role, waiting for QUIC and relay shutdown if it was the last.
    pub(crate) async fn release(self: Arc<Self>) {
        if let Some(last) = Arc::into_inner(self) {
            let drained = last.drained.clone();
            drop(last);
            drained.cancelled().await;
        }
    }
}

impl NetworkConfig {
    /// Select loopback-only QUIC with no relay, discovery or public network services.
    pub fn direct_only() -> Self {
        Self::Loopback
    }

    /// Parse the desktop's optional relay override, including the isolated direct test setting.
    pub fn from_relay_url(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::Public),
            Some("direct") => Ok(Self::Loopback),
            Some(url) => {
                // Validate before iroh normalizes (and may log) the URL.
                let relay: url::Url =
                    url.parse().context("Invalid relay URL")?;
                ensure!(
                    matches!(relay.scheme(), "http" | "https")
                        && relay.username().is_empty()
                        && relay.password().is_none()
                        && relay.query().is_none()
                        && relay.fragment().is_none(),
                    "Invalid relay URL"
                );
                Ok(Self::Relay(relay.into()))
            }
        }
    }

    /// Preserve the actual home relay once known. During slow registration,
    /// carry the configured relay candidates so a copied code remains usable later.
    pub(crate) fn contact_hints(&self, addr: EndpointAddr) -> EndpointAddr {
        match self {
            Self::Relay(url) => addr.with_relay_url(url.clone()),
            Self::Public if addr.relay_urls().next().is_none() => addr
                .with_addrs(
                    default_relay_mode()
                        .relay_map()
                        .urls::<Vec<_>>()
                        .into_iter()
                        .map(TransportAddr::Relay),
                ),
            _ => addr,
        }
    }

    /// Bind with no advertised application protocol until the host is explicitly started.
    pub(crate) async fn bind(
        self,
        profile: DeviceProfile,
        key: &SecretKey,
        loopback_port: u16,
    ) -> Result<SharedEndpoint> {
        let deadline = Instant::now() + Duration::from_secs(2);
        tracing::debug!("Binding remote endpoint {}", key.public().fmt_short());
        let endpoint = loop {
            let builder = match &self {
                Self::Loopback => Endpoint::builder(presets::Minimal)
                    .clear_ip_transports()
                    .bind_addr(SocketAddrV4::new(
                        Ipv4Addr::LOCALHOST,
                        loopback_port,
                    ))?
                    .relay_mode(RelayMode::Disabled),
                Self::Public => Endpoint::builder(presets::N0),
                Self::Relay(url) => Endpoint::builder(presets::N0)
                    .relay_mode(RelayMode::custom([url.clone()])),
            };
            match builder.secret_key(key.clone()).bind().await {
                Ok(endpoint) => break endpoint,
                // iroh releases the UDP socket asynchronously after the last connection.
                // Allow a just-disabled host to rebind its saved loopback route immediately.
                Err(iroh::endpoint::BindError::Sockets { source, .. })
                    if self == Self::Loopback
                        && loopback_port != 0
                        && source.kind() == ErrorKind::AddrInUse
                        && Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(50)).await;
                }
                Err(error) => {
                    tracing::warn!(
                        "Could not bind remote endpoint {}: {error}",
                        key.public().fmt_short()
                    );
                    return Err(error)
                        .context("Could not bind remote endpoint");
                }
            }
        };
        if self != Self::Loopback {
            // LAN discovery is optional on machines without multicast support.
            match MdnsAddressLookup::builder().build(endpoint.id()) {
                Ok(mdns) => endpoint.address_lookup()?.add(mdns),
                Err(_) => tracing::warn!(
                    "LAN discovery unavailable for endpoint {}",
                    endpoint.id().fmt_short()
                ),
            }
        }
        tracing::info!("Bound remote endpoint {}", endpoint.id().fmt_short());
        // Spawn on the owning runtime. The last role releases this worker even if
        // a dial is cancelled, a native object is dropped, or its handshake fails.
        let (lifetime, idle) = oneshot::channel::<()>();
        let drained = CancellationToken::new();
        let completion = drained.clone();
        let owned_endpoint = endpoint.clone();
        tokio::spawn(async move {
            let _ = idle.await;
            owned_endpoint.close().await;
            tracing::debug!(
                "Drained remote endpoint {}",
                owned_endpoint.id().fmt_short()
            );
            drop(owned_endpoint);
            completion.cancel();
            // Keep the installation lock until the endpoint has released its identity.
            drop(profile);
        });
        Ok(SharedEndpoint::builder()
            .endpoint(endpoint)
            .hosting(AtomicBool::new(false))
            .config(self)
            ._lifetime(lifetime)
            .drained(drained)
            .build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{EndpointAddr, SecretKey};

    /// Invitations preserve the selected home relay, or include configured routes before registration.
    #[test]
    fn relay_hints_survive_slow_registration_and_respect_the_override()
    -> Result<()> {
        let addr = EndpointAddr::new(SecretKey::generate().public());
        let custom =
            NetworkConfig::from_relay_url(Some("https://relay.example.org/"))?;
        let configured = custom.contact_hints(addr.clone());
        ensure!(configured.relay_urls().count() == 1);
        ensure!(
            configured
                .relay_urls()
                .next()
                .context("missing custom relay")?
                .as_str()
                == "https://relay.example.org/"
        );
        ensure!(
            NetworkConfig::default()
                .contact_hints(addr.clone())
                .relay_urls()
                .next()
                .is_some()
        );
        ensure!(
            NetworkConfig::default().contact_hints(configured.clone())
                == configured
        );
        ensure!(
            NetworkConfig::direct_only().contact_hints(addr.clone()) == addr
        );
        Ok(())
    }
}
