use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{McpServerId, SessionId, TimestampMs};

/// Public OAuth lifecycle state without token or credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpOAuthState {
    /// This Server does not use OAuth.
    NotConfigured,
    /// OAuth is configured but no usable authorization exists.
    Unauthenticated,
    /// User authorization is currently in progress.
    Authorizing,
    /// A usable access grant is available.
    Ready,
    /// The current grant requires refresh or additional scope.
    RefreshRequired,
    /// OAuth discovery, authorization, or refresh failed.
    Failed,
}

/// Safe OAuth status projected to Kernel, ACP, and WebUI.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthStatus {
    /// Current public OAuth lifecycle state.
    pub state: McpOAuthState,
    /// Granted or requested scopes safe to display.
    #[serde(default)]
    #[builder(default)]
    pub scopes: Vec<String>,
    /// Access grant expiration time when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub expires_at_ms: Option<TimestampMs>,
    /// User-facing authorization URL when interaction is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub authorization_url: Option<String>,
}

impl Default for McpOAuthStatus {
    /// Creates the status used by Servers without OAuth configuration.
    fn default() -> Self {
        Self::builder().state(McpOAuthState::NotConfigured).build()
    }
}

/// User-facing request to continue OAuth authorization for one Session Server.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthorizationRequest {
    /// Server that requires authorization.
    pub server_id: McpServerId,
    /// Session that owns the independent Server connection.
    pub session_id: SessionId,
    /// Validated authorization endpoint URL.
    pub authorization_url: String,
    /// Requested OAuth scopes.
    #[serde(default)]
    #[builder(default)]
    pub scopes: Vec<String>,
}

/// Sensitive authorization callback returned only to the OAuth state machine.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthorizationResult {
    /// Complete callback URI whose credential-like query values must be redacted in logs.
    pub response_uri: String,
}

/// Session-routed ACP request that completes one retained OAuth browser round.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAuthorizationContinueRequest {
    /// Session that owns the retained OAuth state.
    pub session_id: SessionId,
    /// Server that issued the authorization URL.
    pub server_id: McpServerId,
    /// Credential-bearing browser callback accepted only by the OAuth state machine.
    pub result: McpAuthorizationResult,
}

impl fmt::Debug for McpAuthorizationContinueRequest {
    /// Redacts the nested callback URI while retaining safe routing identity.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpAuthorizationContinueRequest")
            .field("session_id", &self.session_id)
            .field("server_id", &self.server_id)
            .field("result", &self.result)
            .finish()
    }
}

impl fmt::Debug for McpAuthorizationResult {
    /// Redacts the credential-bearing callback URI from diagnostic output.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpAuthorizationResult")
            .field("response_uri", &"<redacted>")
            .finish()
    }
}
