//! Device identity, local-owner pairing and authenticated ACP line transport.

mod client;
mod host;
mod network;
mod pairing;
mod profile;
mod session;

pub use client::{PendingPairing, RemoteClient};
pub use host::{HostEvent, HostSnapshot, RemoteHost};
pub use network::NetworkConfig;
pub use pairing::Invitation;
pub use profile::{DeviceProfile, Peer};
pub use session::{RemoteSession, SessionConnection};
