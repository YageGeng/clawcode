use std::{
    env, fs,
    path::{Path, PathBuf},
};

use base64::Engine;
use chrono::{DateTime, Utc};
use http::{HeaderMap, HeaderValue, header::HeaderName};
use reqwest::Client;
use reqwest::header::{
    CONTENT_TYPE, HeaderMap as ReqwestHeaderMap, HeaderValue as ReqwestHeaderValue, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use snafu::{OptionExt, ResultExt, Snafu};
use tokio::sync::{Mutex, RwLock};

/// Default ChatGPT/Codex authorization endpoint.
pub const OPENAI_CODEX_AUTH_ENDPOINT: &str = "https://auth.openai.com";
/// Default ChatGPT/Codex Responses API base URL.
pub const OPENAI_CODEX_API_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// OpenAI's public Codex OAuth client ID.
pub const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Default Codex model name used when the caller does not specify one.
pub const OPENAI_CODEX_DEFAULT_MODEL: &str = "gpt-5.3-codex";

/// Errors produced while establishing or refreshing a Codex session.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum CodexAuthError {
    /// Wraps filesystem failures while persisting or loading session state.
    #[snafu(display("io error at {stage}: {source}"))]
    Io {
        source: std::io::Error,
        stage: String,
    },
    /// Wraps request failures talking to the Codex auth servers.
    #[snafu(display("request error at {stage}: {source}"))]
    Request {
        source: reqwest::Error,
        stage: String,
    },
    /// Wraps JSON parsing and serialization failures.
    #[snafu(display("json error at {stage}: {source}"))]
    Json {
        source: serde_json::Error,
        stage: String,
    },
    /// Wraps header construction failures when configuring the HTTP client.
    #[snafu(display("header error at {stage}: {source}"))]
    Header {
        source: reqwest::header::InvalidHeaderValue,
        stage: String,
    },
    /// Wraps HTTP client creation failures.
    #[snafu(display("http client error at {stage}: {source}"))]
    HttpClient {
        source: reqwest::Error,
        stage: String,
    },
    /// Reports malformed or incomplete JWT access tokens.
    #[snafu(display("invalid token at {stage}: {message}"))]
    InvalidToken { stage: String, message: String },
    /// Reports protocol-level failures returned by the auth server.
    #[snafu(display("protocol error at {stage}: {message}"))]
    Protocol { stage: String, message: String },
}

type Result<T> = std::result::Result<T, CodexAuthError>;

/// Runtime configuration for ChatGPT/Codex authentication.
#[derive(Debug, Clone)]
pub struct OpenAiCodexConfig {
    /// Model name to use when the caller selects Codex mode.
    pub model: String,
    /// OAuth authorization server root URL.
    pub auth_endpoint: String,
    /// Responses API base URL for Codex requests.
    pub api_base_url: String,
    /// Public OAuth client ID used for device-code login.
    pub client_id: String,
    /// Session file path used to persist the refreshable login state.
    pub session_path: PathBuf,
    /// Refresh access tokens this many seconds before expiry.
    pub token_refresh_margin_secs: u64,
}

impl Default for OpenAiCodexConfig {
    /// Builds the default Codex configuration for local CLI usage.
    fn default() -> Self {
        Self {
            model: OPENAI_CODEX_DEFAULT_MODEL.to_string(),
            auth_endpoint: OPENAI_CODEX_AUTH_ENDPOINT.to_string(),
            api_base_url: OPENAI_CODEX_API_BASE_URL.to_string(),
            client_id: OPENAI_CODEX_CLIENT_ID.to_string(),
            session_path: default_session_path(),
            token_refresh_margin_secs: 300,
        }
    }
}

/// Persisted Codex OAuth session data.
#[derive(Serialize, Deserialize)]
pub struct OpenAiCodexSession {
    /// Current bearer access token.
    pub access_token: String,
    /// Long-lived refresh token.
    pub refresh_token: String,
    /// Timestamp when the current access token expires.
    pub expires_at: DateTime<Utc>,
    /// Timestamp when the session was first created.
    pub created_at: DateTime<Utc>,
}

impl std::fmt::Debug for OpenAiCodexSession {
    /// Redacts tokens so test failures and logs never leak credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCodexSession")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Request body for acquiring a device code from OpenAI.
#[derive(Debug, Serialize)]
struct UserCodeRequest {
    /// OAuth client identifier.
    client_id: String,
}

/// Response returned by OpenAI's device-code bootstrap endpoint.
#[derive(Debug, Deserialize)]
struct UserCodeResponse {
    /// Unique device-auth session identifier.
    device_auth_id: String,
    /// Code the user enters in their browser.
    user_code: String,
    /// Verification URL shown to the user.
    #[serde(default = "default_verification_uri")]
    verification_uri: String,
    /// Polling interval in seconds.
    #[serde(
        default = "default_interval",
        deserialize_with = "deserialize_string_or_u64"
    )]
    interval: u64,
    /// Optional ISO-8601 expiration timestamp.
    #[serde(default)]
    expires_at: Option<String>,
    /// Optional lifetime in seconds.
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Request body used while polling for an authorization code.
#[derive(Debug, Serialize)]
struct DeviceTokenPollRequest {
    /// Device-auth session identifier returned by the bootstrap endpoint.
    device_auth_id: String,
    /// User code entered in the browser.
    user_code: String,
}

/// Successful poll response that carries the authorization code and PKCE verifier.
#[derive(Debug, Deserialize)]
struct DeviceAuthCodeResponse {
    /// Short-lived authorization code exchanged for tokens.
    authorization_code: String,
    /// PKCE verifier required by the token exchange.
    code_verifier: String,
}

/// OAuth token response returned by the final exchange and refresh endpoints.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    /// Access token used for Codex API requests.
    access_token: String,
    /// Refresh token used to renew the session.
    #[serde(default)]
    refresh_token: String,
    /// Lifetime of the access token in seconds.
    #[serde(default)]
    expires_in: u64,
}

/// Token block used by the Codex CLI `~/.codex/auth.json` file.
#[derive(Debug, Deserialize)]
struct CodexCliAuthTokens {
    /// Current bearer access token.
    #[serde(default)]
    access_token: Option<String>,
    /// Long-lived refresh token used for silent renewal.
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Top-level Codex CLI auth file shape, with unknown fields preserved separately by callers.
#[derive(Debug, Deserialize)]
struct CodexCliAuthFile {
    /// OAuth token payload persisted by the Codex CLI.
    #[serde(default)]
    tokens: Option<CodexCliAuthTokens>,
}

/// Coordinates persisted Codex login state and silent token refresh.
pub struct OpenAiCodexSessionManager {
    /// Configuration shared by all auth operations.
    config: OpenAiCodexConfig,
    /// HTTP client used for auth requests.
    client: Client,
    /// Cached in-memory session, if available.
    session: RwLock<Option<OpenAiCodexSession>>,
    /// Serializes login and refresh flows so only one renewal happens at a time.
    renewal_lock: Mutex<()>,
}

impl OpenAiCodexSessionManager {
    /// Creates a new session manager and eagerly attempts to load a persisted session.
    pub fn new(config: OpenAiCodexConfig) -> Result<Self> {
        let mut headers = ReqwestHeaderMap::new();
        headers.insert(
            USER_AGENT,
            ReqwestHeaderValue::from_static(concat!("clawcode/", env!("CARGO_PKG_VERSION"))),
        );
        let client = Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context(HttpClientSnafu {
                stage: "build-codex-auth-client".to_string(),
            })?;

        let manager = Self {
            config,
            client,
            session: RwLock::new(None),
            renewal_lock: Mutex::new(()),
        };

        manager.load_session_if_present()?;
        Ok(manager)
    }

    /// Returns the access token, refreshing or authenticating if needed.
    pub async fn get_access_token(&self) -> Result<String> {
        self.ensure_authenticated().await?;
        let guard = self.session.read().await;
        let session = guard.as_ref().context(ProtocolSnafu {
            stage: "read-access-token".to_string(),
            message: "missing session after authentication".to_string(),
        })?;
        Ok(session.access_token.clone())
    }

    /// Ensures the manager has a valid session, refreshing or logging in as needed.
    pub async fn ensure_authenticated(&self) -> Result<()> {
        if self.needs_refresh().await {
            let has_refresh = self
                .session
                .read()
                .await
                .as_ref()
                .map(|session| !session.refresh_token.is_empty())
                .unwrap_or(false);

            if has_refresh && self.refresh_tokens().await.is_ok() {
                return Ok(());
            }

            return self.device_code_login().await;
        }

        Ok(())
    }

    /// Reports whether the current session is absent or close enough to expiry to refresh.
    pub async fn needs_refresh(&self) -> bool {
        let guard = self.session.read().await;
        match guard.as_ref() {
            None => true,
            Some(session) => {
                let refresh_margin =
                    chrono::Duration::seconds(self.config.token_refresh_margin_secs as i64);
                Utc::now() + refresh_margin >= session.expires_at
            }
        }
    }

    /// Runs OpenAI's device-code login flow and persists the resulting session.
    pub async fn device_code_login(&self) -> Result<()> {
        let _renewal_guard = self.renewal_lock.lock().await;

        // Another caller may already have completed authentication while we waited
        // for the lock, so avoid issuing another interactive login sequence.
        if !self.needs_refresh().await {
            return Ok(());
        }

        let auth_base = format!("{}/api/accounts", self.config.auth_endpoint);

        let device = self.request_device_code(&auth_base).await?;
        print_device_code_instructions(&device);
        let auth_code = self
            .poll_for_authorization_code(&auth_base, &device)
            .await?;
        let session = self.exchange_authorization_code(&auth_code).await?;
        self.persist_session(session).await?;

        println!();
        println!("Authentication successful!");
        println!();
        Ok(())
    }

    /// Refreshes the current access token using the saved refresh token.
    pub async fn refresh_tokens(&self) -> Result<()> {
        let _renewal_guard = self.renewal_lock.lock().await;

        if !self.needs_refresh().await {
            return Ok(());
        }

        let refresh_token = {
            let guard = self.session.read().await;
            let session = guard.as_ref().context(ProtocolSnafu {
                stage: "read-refresh-token".to_string(),
                message: "missing session during refresh".to_string(),
            })?;
            session.refresh_token.clone()
        };

        let form_body = serde_urlencoded::to_string([
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", self.config.client_id.as_str()),
        ])
        .map_err(|source| CodexAuthError::Protocol {
            stage: "refresh-token-form-encode".to_string(),
            message: source.to_string(),
        })?;

        let response = self
            .client
            .post(format!("{}/oauth/token", self.config.auth_endpoint))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .context(RequestSnafu {
                stage: "refresh-token-send".to_string(),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.context(RequestSnafu {
                stage: "refresh-token-error-body".to_string(),
            })?;
            return Err(CodexAuthError::Protocol {
                stage: "refresh-token-status".to_string(),
                message: format!("token refresh failed: HTTP {status} -- {body}"),
            });
        }

        let token_response: TokenResponse = response.json().await.context(RequestSnafu {
            stage: "refresh-token-json".to_string(),
        })?;
        let refresh_token = resolve_refresh_token(token_response.refresh_token, &refresh_token);
        let session = build_session(
            token_response.access_token,
            refresh_token,
            token_response.expires_in,
        );
        self.persist_session(session).await
    }

    /// Injects a session directly, primarily for tests.
    #[cfg(test)]
    pub async fn set_session(&self, session: OpenAiCodexSession) {
        let mut guard = self.session.write().await;
        *guard = Some(session);
    }

    /// Loads a session from disk if the configured file already exists.
    fn load_session_if_present(&self) -> Result<()> {
        if !self.config.session_path.exists() {
            return Ok(());
        }

        let data = fs::read_to_string(&self.config.session_path).context(IoSnafu {
            stage: "read-session-file".to_string(),
        })?;
        let Some(session) = parse_session_file(&data)? else {
            return Ok(());
        };

        if let Ok(mut guard) = self.session.try_write() {
            *guard = Some(session);
        }

        Ok(())
    }

    /// Requests the initial device-code payload that the user will confirm in a browser.
    async fn request_device_code(&self, auth_base: &str) -> Result<UserCodeResponse> {
        let response = self
            .client
            .post(format!("{auth_base}/deviceauth/usercode"))
            .json(&UserCodeRequest {
                client_id: self.config.client_id.clone(),
            })
            .send()
            .await
            .context(RequestSnafu {
                stage: "device-code-send".to_string(),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.context(RequestSnafu {
                stage: "device-code-error-body".to_string(),
            })?;
            return Err(CodexAuthError::Protocol {
                stage: "device-code-status".to_string(),
                message: format!("device code request failed: HTTP {status} -- {body}"),
            });
        }

        response.json().await.context(RequestSnafu {
            stage: "device-code-json".to_string(),
        })
    }

    /// Polls until the browser login completes and OpenAI returns an authorization code.
    async fn poll_for_authorization_code(
        &self,
        auth_base: &str,
        device: &UserCodeResponse,
    ) -> Result<DeviceAuthCodeResponse> {
        let poll_url = format!("{auth_base}/deviceauth/token");
        let mut interval = std::time::Duration::from_secs(device.interval.max(5));
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(device.expires_in_secs());

        loop {
            tokio::time::sleep(interval).await;

            if tokio::time::Instant::now() >= deadline {
                return Err(CodexAuthError::Protocol {
                    stage: "device-code-timeout".to_string(),
                    message: "device code authorization timed out".to_string(),
                });
            }

            let response = self
                .client
                .post(&poll_url)
                .json(&DeviceTokenPollRequest {
                    device_auth_id: device.device_auth_id.clone(),
                    user_code: device.user_code.clone(),
                })
                .send()
                .await
                .context(RequestSnafu {
                    stage: "device-token-poll-send".to_string(),
                })?;

            let status = response.status();
            if status.is_success() {
                return response.json().await.context(RequestSnafu {
                    stage: "device-token-poll-json".to_string(),
                });
            }

            // OpenAI returns 403 while authorization is still pending.
            if status == reqwest::StatusCode::FORBIDDEN {
                continue;
            }

            // Slow down on repeated rate-limit responses to avoid hammering the endpoint.
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                interval = (interval + std::time::Duration::from_secs(5))
                    .min(std::time::Duration::from_secs(60));
                continue;
            }

            let body = response.text().await.context(RequestSnafu {
                stage: "device-token-poll-error-body".to_string(),
            })?;
            return Err(CodexAuthError::Protocol {
                stage: "device-token-poll-status".to_string(),
                message: format!("device auth poll failed: HTTP {status} -- {body}"),
            });
        }
    }

    /// Exchanges the authorization code for access and refresh tokens.
    async fn exchange_authorization_code(
        &self,
        auth_code: &DeviceAuthCodeResponse,
    ) -> Result<OpenAiCodexSession> {
        let redirect_uri = format!("{}/deviceauth/callback", self.config.auth_endpoint);
        let form_body = serde_urlencoded::to_string([
            ("grant_type", "authorization_code"),
            ("code", auth_code.authorization_code.as_str()),
            ("code_verifier", auth_code.code_verifier.as_str()),
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .map_err(|source| CodexAuthError::Protocol {
            stage: "exchange-authorization-code-form-encode".to_string(),
            message: source.to_string(),
        })?;
        let response = self
            .client
            .post(format!("{}/oauth/token", self.config.auth_endpoint))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .context(RequestSnafu {
                stage: "exchange-authorization-code-send".to_string(),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.context(RequestSnafu {
                stage: "exchange-authorization-code-error-body".to_string(),
            })?;
            return Err(CodexAuthError::Protocol {
                stage: "exchange-authorization-code-status".to_string(),
                message: format!("token exchange failed: HTTP {status} -- {body}"),
            });
        }

        let token_response: TokenResponse = response.json().await.context(RequestSnafu {
            stage: "exchange-authorization-code-json".to_string(),
        })?;
        Ok(build_session(
            token_response.access_token,
            token_response.refresh_token,
            token_response.expires_in,
        ))
    }

    /// Persists the session on disk and updates the in-memory copy atomically from the caller's view.
    async fn persist_session(&self, session: OpenAiCodexSession) -> Result<()> {
        save_session(&self.config.session_path, &session)?;
        let mut guard = self.session.write().await;
        *guard = Some(session);
        Ok(())
    }
}

/// Builds the Codex-specific default headers that must accompany bearer auth.
pub fn build_codex_headers(access_token: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let account_id = extract_chatgpt_account_id(access_token)?;
    headers.insert(
        HeaderName::from_static("chatgpt-account-id"),
        HeaderValue::from_str(&account_id).map_err(|source| CodexAuthError::Header {
            stage: "build-chatgpt-account-id-header".to_string(),
            source,
        })?,
    );
    headers.insert(
        HeaderName::from_static("openai-beta"),
        HeaderValue::from_static("responses=experimental"),
    );
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_static("clawcode"),
    );
    Ok(headers)
}

/// Extracts the ChatGPT account identifier embedded in the access token JWT claims.
pub fn extract_chatgpt_account_id(token: &str) -> Result<String> {
    let payload_b64 = token.split('.').nth(1).context(InvalidTokenSnafu {
        stage: "split-jwt".to_string(),
        message: "JWT token has fewer than 2 parts".to_string(),
    })?;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let decoded = engine
        .decode(payload_b64)
        .map_err(|source| CodexAuthError::InvalidToken {
            stage: "decode-jwt-payload".to_string(),
            message: source.to_string(),
        })?;
    let payload: serde_json::Value = serde_json::from_slice(&decoded).context(JsonSnafu {
        stage: "parse-jwt-payload".to_string(),
    })?;
    payload
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .context(InvalidTokenSnafu {
            stage: "extract-chatgpt-account-id".to_string(),
            message: "JWT payload missing chatgpt_account_id claim".to_string(),
        })
}

/// Produces the default Codex CLI auth path under `~/.codex/`.
fn default_session_path() -> PathBuf {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("auth.json")
}

/// Returns the default browser verification URL when the API omits one.
fn default_verification_uri() -> String {
    "https://auth.openai.com/codex/device".to_string()
}

/// Returns the minimum polling interval when the server omits one.
fn default_interval() -> u64 {
    5
}

/// Parses a JSON field that OpenAI may encode either as a string or a number.
fn deserialize_string_or_u64<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrU64;

    impl<'de> de::Visitor<'de> for StringOrU64 {
        type Value = u64;

        /// Describes the accepted input shape for serde diagnostics.
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or integer")
        }

        /// Accepts JSON integers directly.
        fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<u64, E> {
            Ok(value)
        }

        /// Accepts stringly-typed integers used by OpenAI's endpoint.
        fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<u64, E> {
            value.parse().map_err(de::Error::custom)
        }
    }

    deserializer.deserialize_any(StringOrU64)
}

impl UserCodeResponse {
    /// Computes the remaining lifetime of the device code in seconds.
    fn expires_in_secs(&self) -> u64 {
        if let Some(seconds) = self.expires_in {
            return seconds;
        }

        if let Some(timestamp) = self.expires_at.as_ref()
            && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp)
        {
            let remaining_seconds = parsed.signed_duration_since(Utc::now()).num_seconds();
            return remaining_seconds.max(0) as u64;
        }

        900
    }
}

/// Prints the browser instructions for the device-code login flow.
fn print_device_code_instructions(device: &UserCodeResponse) {
    println!();
    println!("===========================================================");
    println!("               OpenAI Codex Authentication                  ");
    println!("===========================================================");
    println!();
    println!("  1. Open this URL in any browser:");
    println!("     {}", device.verification_uri);
    println!();
    println!("  2. Enter this code:");
    println!();
    println!("              [  {}  ]", device.user_code);
    println!();
    println!(
        "  Waiting for authorization... (expires in {} min)",
        device.expires_in_secs() / 60
    );
    println!("===========================================================");
    println!();
}

/// Converts OAuth token fields into the persisted session representation.
fn build_session(
    access_token: String,
    refresh_token: String,
    expires_in: u64,
) -> OpenAiCodexSession {
    // The endpoint occasionally omits a useful TTL, so fall back to a conservative one-hour lifetime.
    let effective_expires_in = if expires_in > 0 { expires_in } else { 3600 };
    OpenAiCodexSession {
        access_token,
        refresh_token,
        expires_at: Utc::now() + chrono::Duration::seconds(effective_expires_in as i64),
        created_at: Utc::now(),
    }
}

/// Parses either the Codex CLI auth file shape or the legacy flat session shape.
fn parse_session_file(data: &str) -> Result<Option<OpenAiCodexSession>> {
    let value = serde_json::from_str::<Value>(data).context(JsonSnafu {
        stage: "parse-session-json".to_string(),
    })?;

    if value.get("tokens").is_some() || value.get("auth_mode").is_some() {
        return session_from_codex_cli_auth_value(value);
    }

    if value.get("access_token").is_none() && value.get("refresh_token").is_none() {
        return Ok(None);
    }

    serde_json::from_value::<OpenAiCodexSession>(value)
        .map(Some)
        .context(JsonSnafu {
            stage: "parse-legacy-session-json".to_string(),
        })
}

/// Converts Codex CLI auth JSON into the internal session representation.
fn session_from_codex_cli_auth_value(value: Value) -> Result<Option<OpenAiCodexSession>> {
    let auth_file = serde_json::from_value::<CodexCliAuthFile>(value).context(JsonSnafu {
        stage: "parse-codex-cli-auth-json".to_string(),
    })?;
    let Some(tokens) = auth_file.tokens else {
        return Ok(None);
    };

    let access_token = tokens.access_token.unwrap_or_default().trim().to_string();
    let refresh_token = tokens.refresh_token.unwrap_or_default().trim().to_string();

    if access_token.is_empty() && refresh_token.is_empty() {
        return Ok(None);
    }

    // Treat tokens without a readable expiry as stale so a refresh token can repair them.
    let expires_at = extract_jwt_expiry(&access_token).unwrap_or_else(Utc::now);
    Ok(Some(OpenAiCodexSession {
        access_token,
        refresh_token,
        expires_at,
        created_at: Utc::now(),
    }))
}

/// Extracts the JWT `exp` claim without failing the broader auth loading path.
fn extract_jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let payload_b64 = token.split('.').nth(1)?;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let decoded = engine.decode(payload_b64).ok()?;
    let payload: Value = serde_json::from_slice(&decoded).ok()?;
    let exp = payload.get("exp")?.as_i64()?;
    DateTime::<Utc>::from_timestamp(exp, 0)
}

/// Keeps the previous refresh token when the server returns an empty token.
fn resolve_refresh_token(new_refresh_token: String, previous_refresh_token: &str) -> String {
    if new_refresh_token.is_empty() {
        previous_refresh_token.to_string()
    } else {
        new_refresh_token
    }
}

/// Saves the current session to disk using restrictive permissions on Unix platforms.
fn save_session(path: &Path, session: &OpenAiCodexSession) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context(IoSnafu {
            stage: "create-session-directory".to_string(),
        })?;
    }

    let auth_json = codex_cli_auth_json_for_session(path, session)?;
    let json = serde_json::to_string_pretty(&auth_json).context(JsonSnafu {
        stage: "serialize-session".to_string(),
    })?;
    fs::write(path, json).context(IoSnafu {
        stage: "write-session-file".to_string(),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).context(IoSnafu {
            stage: "chmod-session-file".to_string(),
        })?;
    }

    Ok(())
}

/// Builds the Codex CLI auth JSON to write, preserving unrelated existing fields.
fn codex_cli_auth_json_for_session(path: &Path, session: &OpenAiCodexSession) -> Result<Value> {
    let mut value = if path.exists() {
        let data = fs::read_to_string(path).context(IoSnafu {
            stage: "read-existing-auth-file".to_string(),
        })?;
        serde_json::from_str::<Value>(&data).context(JsonSnafu {
            stage: "parse-existing-auth-file".to_string(),
        })?
    } else {
        Value::Object(Map::new())
    };

    if !value.is_object() {
        value = Value::Object(Map::new());
    }

    let root = value.as_object_mut().context(ProtocolSnafu {
        stage: "prepare-auth-json-root".to_string(),
        message: "auth JSON root is not an object".to_string(),
    })?;
    root.entry("auth_mode".to_string())
        .or_insert_with(|| Value::String("chatgpt".to_string()));
    root.insert(
        "last_refresh".to_string(),
        Value::String(Utc::now().to_rfc3339()),
    );

    let tokens = root
        .entry("tokens".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !tokens.is_object() {
        *tokens = Value::Object(Map::new());
    }
    let token_object = tokens.as_object_mut().context(ProtocolSnafu {
        stage: "prepare-auth-json-tokens".to_string(),
        message: "auth JSON tokens field is not an object".to_string(),
    })?;

    token_object.insert(
        "access_token".to_string(),
        Value::String(session.access_token.clone()),
    );
    token_object.insert(
        "refresh_token".to_string(),
        Value::String(session.refresh_token.clone()),
    );
    if let Ok(account_id) = extract_chatgpt_account_id(&session.access_token) {
        token_object.insert("account_id".to_string(), Value::String(account_id));
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Builds a structurally valid JWT containing the requested ChatGPT account ID.
    fn make_test_jwt(account_id: &str) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}");
        let payload = engine.encode(
            serde_json::json!({
                "sub": "user123",
                "exp": (Utc::now() + chrono::Duration::hours(1)).timestamp(),
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id,
                },
            })
            .to_string(),
        );
        let signature = engine.encode(b"fake-signature");
        format!("{header}.{payload}.{signature}")
    }

    /// Verifies the account-id claim is read from the JWT payload.
    #[test]
    fn extracts_chatgpt_account_id_from_jwt() {
        let token = make_test_jwt("acct_123");
        assert_eq!(extract_chatgpt_account_id(&token).unwrap(), "acct_123");
    }

    /// Verifies the helper injects the required Codex headers.
    #[test]
    fn builds_codex_headers_from_access_token() {
        let token = make_test_jwt("acct_456");
        let headers = build_codex_headers(&token).unwrap();
        assert_eq!(
            headers.get("chatgpt-account-id").unwrap(),
            &HeaderValue::from_static("acct_456")
        );
        assert_eq!(
            headers.get("openai-beta").unwrap(),
            &HeaderValue::from_static("responses=experimental")
        );
        assert_eq!(
            headers.get("originator").unwrap(),
            &HeaderValue::from_static("clawcode")
        );
    }

    /// Verifies a persisted session is loaded when the manager starts.
    #[tokio::test]
    async fn loads_persisted_session_from_disk() {
        let temp_dir = tempdir().unwrap();
        let session_path = temp_dir.path().join("session.json");
        let access_token = make_test_jwt("acct_abc");
        let session = OpenAiCodexSession {
            access_token: access_token.clone(),
            refresh_token: "refresh_xyz".to_string(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            created_at: Utc::now(),
        };
        save_session(&session_path, &session).unwrap();

        let manager = OpenAiCodexSessionManager::new(OpenAiCodexConfig {
            session_path,
            ..OpenAiCodexConfig::default()
        })
        .unwrap();

        assert_eq!(manager.get_access_token().await.unwrap(), access_token);
    }

    /// Verifies the manager can read the Codex CLI auth file shape without interactive login.
    #[tokio::test]
    async fn loads_codex_cli_auth_json_from_disk() {
        let temp_dir = tempdir().unwrap();
        let session_path = temp_dir.path().join("auth.json");
        let access_token = make_test_jwt("acct_cli");
        fs::write(
            &session_path,
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "id_abc",
                    "access_token": access_token,
                    "refresh_token": "refresh_cli",
                    "account_id": "acct_cli",
                },
                "last_refresh": "2026-04-26T10:00:00Z",
                "unknown_field": {
                    "keep": true,
                },
            })
            .to_string(),
        )
        .unwrap();

        let manager = OpenAiCodexSessionManager::new(OpenAiCodexConfig {
            session_path,
            ..OpenAiCodexConfig::default()
        })
        .unwrap();

        assert_eq!(manager.get_access_token().await.unwrap(), access_token);
    }

    /// Verifies an auth file without OAuth tokens does not block first-time browser auth.
    #[tokio::test]
    async fn ignores_codex_cli_auth_json_without_tokens() {
        let temp_dir = tempdir().unwrap();
        let session_path = temp_dir.path().join("auth.json");
        fs::write(
            &session_path,
            serde_json::json!({
                "auth_mode": "apikey",
                "unknown_field": {
                    "keep": true,
                },
            })
            .to_string(),
        )
        .unwrap();

        let manager = OpenAiCodexSessionManager::new(OpenAiCodexConfig {
            session_path,
            ..OpenAiCodexConfig::default()
        })
        .expect("auth files without OAuth tokens should be treated as missing sessions");

        assert!(manager.needs_refresh().await);
    }

    /// Verifies saving a session updates Codex CLI token fields without discarding unknown fields.
    #[test]
    fn save_session_preserves_codex_cli_auth_json_fields() {
        let temp_dir = tempdir().unwrap();
        let session_path = temp_dir.path().join("auth.json");
        fs::write(
            &session_path,
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "id_old",
                    "access_token": "access_old",
                    "refresh_token": "refresh_old",
                    "account_id": "acct_old",
                },
                "last_refresh": "2026-04-26T10:00:00Z",
                "unknown_field": {
                    "keep": true,
                },
            })
            .to_string(),
        )
        .unwrap();
        let access_token = make_test_jwt("acct_new");
        let session = OpenAiCodexSession {
            access_token: access_token.clone(),
            refresh_token: "refresh_new".to_string(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            created_at: Utc::now(),
        };

        save_session(&session_path, &session).unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&session_path).unwrap()).unwrap();
        assert_eq!(updated["auth_mode"], "chatgpt");
        assert_eq!(updated["tokens"]["id_token"], "id_old");
        assert_eq!(updated["tokens"]["access_token"], access_token);
        assert_eq!(updated["tokens"]["refresh_token"], "refresh_new");
        assert_eq!(updated["tokens"]["account_id"], "acct_new");
        assert_eq!(updated["unknown_field"]["keep"], true);
        assert!(updated["last_refresh"].as_str().is_some());
    }

    /// Verifies the refresh heuristic reports stale sessions correctly.
    #[tokio::test]
    async fn reports_expiring_sessions_as_needing_refresh() {
        let manager = OpenAiCodexSessionManager::new(OpenAiCodexConfig::default()).unwrap();
        manager
            .set_session(OpenAiCodexSession {
                access_token: "access_abc".to_string(),
                refresh_token: "refresh_xyz".to_string(),
                expires_at: Utc::now() + chrono::Duration::seconds(30),
                created_at: Utc::now(),
            })
            .await;

        assert!(manager.needs_refresh().await);
    }

    /// Keeps the old refresh token when refresh responses omit a new value.
    #[test]
    fn keeps_previous_refresh_token_when_refresh_token_is_empty() {
        assert_eq!(
            resolve_refresh_token("".to_string(), "refresh_old"),
            "refresh_old".to_string()
        );
    }

    /// Replaces the old refresh token when the server returns a fresh value.
    #[test]
    fn replaces_refresh_token_when_new_token_is_present() {
        assert_eq!(
            resolve_refresh_token("refresh_new".to_string(), "refresh_old"),
            "refresh_new".to_string()
        );
    }
}
