#![expect(
    clippy::panic,
    reason = "reference server requires exhaustive protocol assertions"
)]

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use mcp::{
    McpAuthentication, McpConnector, McpError, McpHost, McpHttpHeaders,
    McpHttpUrl, McpMrtrPolicy, McpOAuthSettings, McpStreamableHttpTransport,
    McpTransport, RmcpConnector, RuntimeMcpServer,
};
use protocol::{
    McpAuthorizationRequest, McpAuthorizationResult, McpHostRequest,
    McpHostResponse, McpProtocolVersion, McpServerId, SessionId,
};
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, DiscoverResult, Implementation,
    ProtocolVersion, ServerJsonRpcMessage, ServerResult,
};
use store::FileSecretStore;

/// Shared reference-server counters and canonical endpoint identity.
#[derive(Clone)]
struct OAuthServerState {
    base_url: String,
    token_requests: Arc<AtomicUsize>,
}

/// Browser fixture validates the authorization URL and creates one issuer-bound callback.
struct AuthorizationHost {
    issuer: String,
    requests: AtomicUsize,
}

impl AuthorizationHost {
    /// Simulates one browser authorization redirect without exposing callback data.
    fn authorize(
        &self,
        request: &McpAuthorizationRequest,
    ) -> McpAuthorizationResult {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let authorization_url = url::Url::parse(&request.authorization_url)
            .expect("authorization URL");
        let query = authorization_url
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            query.get("resource").map(String::as_str),
            Some(format!("{}/mcp", self.issuer).as_str())
        );
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(query.get("scope").map(String::as_str), Some("tools:read"));
        let redirect_uri = query.get("redirect_uri").expect("redirect_uri");
        let state = query.get("state").expect("state");
        let mut callback = url::Url::parse(redirect_uri).expect("callback URL");
        callback
            .query_pairs_mut()
            .append_pair("code", "authorization-code")
            .append_pair("state", state)
            .append_pair("iss", &self.issuer);
        McpAuthorizationResult {
            response_uri: callback.to_string(),
        }
    }
}

#[async_trait]
impl McpHost for AuthorizationHost {
    /// Rejects unexpected in-band Host callbacks during transport startup.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host("unexpected Host request".to_string()))
    }
}

/// Returns RFC 9728 Protected Resource Metadata for the MCP endpoint.
async fn protected_resource_metadata(
    State(state): State<OAuthServerState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "resource": format!("{}/mcp", state.base_url),
        "authorization_servers": [state.base_url],
        "scopes_supported": ["tools:read"]
    }))
}

/// Returns strict RFC 8414 metadata including PKCE and issuer-response support.
async fn authorization_server_metadata(
    State(state): State<OAuthServerState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": state.base_url,
        "authorization_endpoint": format!("{}/authorize", state.base_url),
        "token_endpoint": format!("{}/token", state.base_url),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "scopes_supported": ["tools:read"]
    }))
}

/// Exchanges a PKCE code while asserting the required MCP resource indicator.
async fn token(
    State(state): State<OAuthServerState>,
    Form(form): Form<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    assert_eq!(
        form.get("resource").map(String::as_str),
        Some(format!("{}/mcp", state.base_url).as_str())
    );
    let grant_type = form.get("grant_type").map(String::as_str);
    match grant_type {
        Some("authorization_code") => assert!(
            form.get("code_verifier")
                .is_some_and(|value| !value.is_empty())
        ),
        Some("refresh_token") => assert_eq!(
            form.get("refresh_token").map(String::as_str),
            Some("refresh-secret")
        ),
        other => panic!("unexpected OAuth grant: {other:?}"),
    }
    state.token_requests.fetch_add(1, Ordering::SeqCst);
    Json(serde_json::json!({
        "access_token": "access-secret",
        "token_type": "Bearer",
        "expires_in": if grant_type == Some("authorization_code") { 1 } else { 3600 },
        "refresh_token": "refresh-secret",
        "scope": "tools:read"
    }))
}

/// Challenges unauthenticated discovery and serves exact Modern MCP responses after OAuth.
async fn mcp_endpoint(
    State(state): State<OAuthServerState>,
    headers: HeaderMap,
    Json(message): Json<ClientJsonRpcMessage>,
) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer access-secret")
    {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_str(&format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\", scope=\"tools:read\"",
                state.base_url
            ))
            .expect("WWW-Authenticate Header"),
        );
        return response;
    }
    let ClientJsonRpcMessage::Request(request) = message else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match request.request {
        ClientRequest::DiscoverRequest(_) => ServerResult::DiscoverResult(
            DiscoverResult::new(
                vec![ProtocolVersion::V_2026_07_28],
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
            .with_server_info(Implementation::new("oauth-server", "1.0.0")),
        ),
        ClientRequest::ListToolsRequest(_) => {
            ServerResult::ListToolsResult(Default::default())
        }
        other => panic!("unexpected MCP request: {other:?}"),
    };
    Json(ServerJsonRpcMessage::response(result, request.id)).into_response()
}

/// GET /mcp advertises the protected-resource metadata location.
async fn mcp_challenge(State(state): State<OAuthServerState>) -> Response {
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\", scope=\"tools:read\"",
            state.base_url
        ))
        .expect("WWW-Authenticate Header"),
    );
    response
}

/// Builds one exact Modern OAuth Server config for the reference endpoint.
fn runtime_server(base_url: &str) -> RuntimeMcpServer {
    RuntimeMcpServer::builder()
        .server_id(McpServerId::try_from("oauth-server").expect("Server id"))
        .enabled(true)
        .protocol(McpProtocolVersion::V2026_07_28)
        .startup_timeout(Duration::from_secs(3))
        .request_timeout(Duration::from_secs(1))
        .mrtr(McpMrtrPolicy {
            max_rounds: NonZeroU32::new(2).expect("non-zero rounds"),
            total_timeout: Duration::from_secs(2),
        })
        .transport(McpTransport::StreamableHttp(Box::new(
            McpStreamableHttpTransport {
                url: McpHttpUrl::try_from(format!("{base_url}/mcp"))
                    .expect("MCP URL"),
                headers: McpHttpHeaders::default(),
                authentication: McpAuthentication::OAuth(Box::new(
                    McpOAuthSettings::builder()
                        .client_id("test-client".to_string())
                        .scopes(vec!["tools:read".to_string()])
                        .redirect_uri(
                            McpHttpUrl::try_from(
                                "http://127.0.0.1:48123/callback".to_string(),
                            )
                            .expect("redirect URL"),
                        )
                        .build(),
                )),
            },
        )))
        .build()
}

/// OAuth discovery, PKCE exchange, bearer injection, and persisted restart all interoperate.
#[tokio::test]
async fn oauth_authorizes_and_reuses_persisted_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reference listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let token_requests = Arc::new(AtomicUsize::new(0));
    let state = OAuthServerState {
        base_url: base_url.clone(),
        token_requests: Arc::clone(&token_requests),
    };
    let app = Router::new()
        .route("/mcp", get(mcp_challenge).post(mcp_endpoint))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/token", post(token))
        .with_state(state);
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("reference server");
    });
    let credentials = tempfile::tempdir().expect("credential directory");
    let connector = RmcpConnector::new(Arc::new(
        FileSecretStore::new(credentials.path()).expect("Secret Store"),
    ));
    let host = Arc::new(AuthorizationHost {
        issuer: base_url.clone(),
        requests: AtomicUsize::new(0),
    });
    let session_id = SessionId::try_from("session-oauth").expect("Session id");
    let config = runtime_server(&base_url);

    let authorization = match connector
        .connect(&config, &session_id, Arc::clone(&host) as Arc<dyn McpHost>)
        .await
    {
        Ok(_) => {
            panic!("first OAuth connection must require browser authorization")
        }
        Err(error) => error,
    };
    let McpError::AuthorizationRequired(request) = authorization else {
        panic!("expected pending authorization");
    };
    let first = connector
        .continue_authorization(
            &config,
            &session_id,
            host.authorize(&request),
            Arc::clone(&host) as Arc<dyn McpHost>,
        )
        .await
        .expect("continued OAuth connection");
    first.discover().await.expect("first catalog");
    first.shutdown().await.expect("first shutdown");
    let second = connector
        .connect(&config, &session_id, Arc::clone(&host) as Arc<dyn McpHost>)
        .await
        .expect("restored OAuth connection");
    second.discover().await.expect("restored catalog");
    second.shutdown().await.expect("second shutdown");

    assert_eq!(host.requests.load(Ordering::SeqCst), 1);
    assert_eq!(token_requests.load(Ordering::SeqCst), 2);
    let credential_count = std::fs::read_dir(credentials.path())
        .expect("credential directory")
        .filter_map(Result::ok)
        .count();
    assert_eq!(credential_count, 1);
    server_task.abort();
}
