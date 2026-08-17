//! MCP OAuth 2.1 orchestration over rmcp's standards-compliant state machine.

use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    McpAuthorizationRequest, McpServerId, ProductIdentity, SessionId,
};
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadataSource,
    AuthorizationRequest, AuthorizationSession, CredentialStore,
    StoredCredentials,
};
use store::{SecretKey, SecretStore, SecretValue};

use crate::{McpError, McpHttpUrl, McpOAuthSettings};

/// Result of restoring credentials or preparing a browser authorization round.
pub(crate) enum McpOAuthGrant {
    /// Persisted credentials are ready for authenticated HTTP requests.
    Ready(AuthorizationManager),
    /// Browser interaction is required while the PKCE session remains retained.
    Pending {
        /// Standards-compliant authorization state retained until ACP continuation.
        session: AuthorizationSession,
        /// Safe browser URL and public scope projection.
        request: McpAuthorizationRequest,
    },
}

/// Adapts the application's restricted Secret Store to rmcp credential persistence.
struct SecretCredentialStore {
    store: Arc<dyn SecretStore>,
    key: SecretKey,
}

impl SecretCredentialStore {
    /// Binds one authorization client identity to a stable Server credential key.
    fn new(
        store: Arc<dyn SecretStore>,
        server_id: McpServerId,
        client_id: &str,
    ) -> Result<Self, McpError> {
        let key = SecretKey::new(server_id, client_id).map_err(|error| {
            McpError::OAuth(format!("Secret key creation failed: {error}"))
        })?;
        Ok(Self { store, key })
    }
}

#[async_trait]
impl CredentialStore for SecretCredentialStore {
    /// Loads and decodes one persisted rmcp credential document off the async runtime.
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let store = Arc::clone(&self.store);
        let key = self.key.clone();
        let value = tokio::task::spawn_blocking(move || store.load(&key))
            .await
            .map_err(|error| AuthError::InternalError(error.to_string()))?
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        value
            .map(|secret| {
                serde_json::from_slice(secret.expose()).map_err(|error| {
                    AuthError::InternalError(format!(
                        "stored OAuth credential is invalid: {error}"
                    ))
                })
            })
            .transpose()
    }

    /// Encodes and atomically persists credentials without logging their contents.
    async fn save(
        &self,
        credentials: StoredCredentials,
    ) -> Result<(), AuthError> {
        let encoded = serde_json::to_vec(&credentials)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        let value = SecretValue::new(encoded)
            .map_err(|error| AuthError::InternalError(error.to_string()))?;
        let store = Arc::clone(&self.store);
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || store.store(&key, &value))
            .await
            .map_err(|error| AuthError::InternalError(error.to_string()))?
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }

    /// Removes credentials after issuer changes or explicit reauthorization.
    async fn clear(&self) -> Result<(), AuthError> {
        let store = Arc::clone(&self.store);
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || store.delete(&key))
            .await
            .map_err(|error| AuthError::InternalError(error.to_string()))?
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }
}

/// One Session/Server OAuth policy with no transport or Kernel dependency.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct McpOAuthRuntime {
    server_id: McpServerId,
    session_id: SessionId,
    resource_url: McpHttpUrl,
    settings: McpOAuthSettings,
    secrets: Arc<dyn SecretStore>,
}

impl McpOAuthRuntime {
    /// Restores a resource-bound grant or prepares a non-blocking browser round.
    pub(crate) async fn authorize(self) -> Result<McpOAuthGrant, McpError> {
        let mut manager = AuthorizationManager::new(self.resource_url.as_str())
            .await
            .map_err(McpError::from_oauth)?;
        manager.set_credential_store(SecretCredentialStore::new(
            Arc::clone(&self.secrets),
            self.server_id.clone(),
            &self.settings.client_id,
        )?);
        let resolution = manager
            .resolve_metadata()
            .await
            .map_err(McpError::from_oauth)?;
        if resolution.source
            == AuthorizationMetadataSource::LegacyEndpointFallback
        {
            return Err(McpError::OAuth(
                "MCP OAuth metadata discovery produced only legacy synthesized endpoints"
                    .to_string(),
            ));
        }
        let mut metadata = resolution.metadata;
        if let Some(url) = &self.settings.authorization_url {
            metadata.authorization_endpoint = url.as_str().to_string();
        }
        if let Some(url) = &self.settings.token_url {
            metadata.token_endpoint = url.as_str().to_string();
        }
        manager.set_metadata(metadata);
        if manager
            .initialize_from_store()
            .await
            .map_err(McpError::from_oauth)?
        {
            return Ok(McpOAuthGrant::Ready(manager));
        }

        let mut request =
            AuthorizationRequest::new(self.settings.redirect_uri.as_str())
                .with_preregistered_client(self.settings.client_id.clone())
                .with_client_name(ProductIdentity::NAME)
                .with_scopes(self.settings.scopes.clone())
                .with_application_type("native");
        if let Some(secret) = &self.settings.client_secret {
            request = request.with_client_secret(secret.clone());
        }
        let session = AuthorizationSession::new(manager, request)
            .await
            .map_err(|(_manager, error)| McpError::from_oauth(error))?;
        let request = McpAuthorizationRequest::builder()
            .server_id(self.server_id)
            .session_id(self.session_id)
            .authorization_url(session.get_authorization_url().to_string())
            .scopes(self.settings.scopes)
            .build();
        Ok(McpOAuthGrant::Pending { session, request })
    }
}
