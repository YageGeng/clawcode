use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Provider-private payload has been serialized and may be replaced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeProviderRequestEvent {
    /// Current provider-private request body.
    pub payload: serde_json::Value,
}

/// Final provider headers have been assembled and may be patched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeforeProviderHeadersEvent {
    /// Normalized non-secret request headers visible to extensions.
    pub headers: BTreeMap<String, String>,
}

/// Provider response metadata is available before stream consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterProviderResponseEvent {
    /// HTTP or WebSocket handshake status.
    pub status: u16,
    /// Normalized response headers with sensitive values removed.
    pub headers: BTreeMap<String, String>,
}
