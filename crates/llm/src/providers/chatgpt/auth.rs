use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use snafu::{ResultExt, Snafu};
use tokio::sync::Mutex;

use crate::providers::openai::codex::{
    CodexAuthError, OpenAiCodexConfig, OpenAiCodexSessionManager, extract_chatgpt_account_id,
};

/// Supported ChatGPT authentication sources.
#[derive(Clone)]
pub enum AuthSource {
    /// Use a caller-supplied access token directly.
    AccessToken { access_token: String },
    /// Reuse the persisted OAuth device-code session flow.
    OAuth,
}

impl fmt::Debug for AuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessToken { .. } => f.write_str("AccessToken(<redacted>)"),
            Self::OAuth => f.write_str("OAuth"),
        }
    }
}

/// Resolved bearer token plus the derived ChatGPT account id.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub access_token: String,
    pub account_id: String,
}

/// Errors raised while resolving ChatGPT authentication state.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("codex auth error on `{stage}`, {source}"))]
    Codex {
        source: CodexAuthError,
        stage: String,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Mutable configuration for the shared ChatGPT OAuth session manager.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub auth_endpoint: String,
    pub api_base_url: String,
    pub client_id: String,
    pub session_path: PathBuf,
    pub token_refresh_margin_secs: u64,
}

impl Default for AuthConfig {
    /// Builds the default ChatGPT auth configuration from the existing Codex defaults.
    fn default() -> Self {
        let config = OpenAiCodexConfig::default();
        Self {
            auth_endpoint: config.auth_endpoint,
            api_base_url: config.api_base_url,
            client_id: config.client_id,
            session_path: config.session_path,
            token_refresh_margin_secs: config.token_refresh_margin_secs,
        }
    }
}

/// Resolves bearer tokens for the ChatGPT provider and caches the OAuth manager lazily.
#[derive(Clone)]
pub struct Authenticator {
    source: AuthSource,
    config: AuthConfig,
    manager: Arc<Mutex<Option<Arc<OpenAiCodexSessionManager>>>>,
}

impl fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authenticator")
            .field("source", &self.source)
            .field("config", &self.config)
            .finish()
    }
}

impl Authenticator {
    /// Creates an authenticator that can use either a static token or a persisted OAuth session.
    pub fn new(source: AuthSource, config: AuthConfig) -> Self {
        Self {
            source,
            config,
            manager: Arc::new(Mutex::new(None)),
        }
    }

    /// Resolves the current access token and derives the ChatGPT account id from its JWT claims.
    pub async fn auth_context(&self) -> Result<AuthContext> {
        match &self.source {
            AuthSource::AccessToken { access_token } => {
                let account_id = extract_chatgpt_account_id(access_token).context(CodexSnafu {
                    stage: "chatgpt-extract-account-id".to_string(),
                })?;
                Ok(AuthContext {
                    access_token: access_token.clone(),
                    account_id,
                })
            }
            AuthSource::OAuth => {
                let manager = self.session_manager().await?;
                let access_token = manager.get_access_token().await.context(CodexSnafu {
                    stage: "chatgpt-get-access-token".to_string(),
                })?;
                let account_id = extract_chatgpt_account_id(&access_token).context(CodexSnafu {
                    stage: "chatgpt-extract-account-id".to_string(),
                })?;
                Ok(AuthContext {
                    access_token,
                    account_id,
                })
            }
        }
    }

    /// Initializes the shared OAuth session manager on first use and reuses it across requests.
    async fn session_manager(&self) -> Result<Arc<OpenAiCodexSessionManager>> {
        let mut manager = self.manager.lock().await;
        if let Some(manager) = manager.as_ref() {
            return Ok(Arc::clone(manager));
        }

        let session_manager = Arc::new(
            OpenAiCodexSessionManager::new(self.openai_codex_config()).context(CodexSnafu {
                stage: "chatgpt-build-session-manager".to_string(),
            })?,
        );
        *manager = Some(Arc::clone(&session_manager));
        Ok(session_manager)
    }

    /// Converts the ChatGPT auth configuration into the shared Codex session-manager config.
    fn openai_codex_config(&self) -> OpenAiCodexConfig {
        OpenAiCodexConfig {
            model: crate::providers::openai::codex::OPENAI_CODEX_DEFAULT_MODEL.to_string(),
            auth_endpoint: self.config.auth_endpoint.clone(),
            api_base_url: self.config.api_base_url.clone(),
            client_id: self.config.client_id.clone(),
            session_path: self.config.session_path.clone(),
            token_refresh_margin_secs: self.config.token_refresh_margin_secs,
        }
    }
}
