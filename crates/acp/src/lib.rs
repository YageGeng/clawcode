//! ACP v2 adaptation and stdio, HTTP/SSE, and WebSocket transports.

mod batching;
mod coalescing;
mod content;
mod extension;
mod input;
mod mapping;
mod projection;
mod recovery;
mod server;
mod terminal;
mod trace;
mod transport;

pub use mapping::{AcpEventMapper, AcpMappingError};
pub use server::AcpServerFactory;
pub use trace::AcpTransportKind;
pub use transport::{AcpTransportError, HttpTransportOptions};
