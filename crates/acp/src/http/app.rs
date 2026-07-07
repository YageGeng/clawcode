//! Axum application assembly for the clawcode ACP HTTP server.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderName, Method, header},
};
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use crate::backend::fs::AcpClientFsRouter;
use crate::backend::terminal::AcpClientTerminalRouter;
use protocol::AgentKernel;

use super::routers::build_router;

/// Options used when serving the ACP agent over HTTP.
#[derive(Debug, Clone)]
pub struct HttpServerOptions {
    /// Socket address used by the HTTP listener.
    pub bind: SocketAddr,
    /// ACP endpoint path mounted into the Axum router.
    pub acp_path: String,
}

impl Default for HttpServerOptions {
    /// Builds local-only defaults for the HTTP server.
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            acp_path: "/acp".to_string(),
        }
    }
}

/// Builds the CORS layer shared by static assets and ACP transport routes.
pub(crate) fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("acp-connection-id"),
            HeaderName::from_static("acp-session-id"),
            header::SEC_WEBSOCKET_VERSION,
            header::SEC_WEBSOCKET_KEY,
            header::CONNECTION,
            header::UPGRADE,
        ])
        .expose_headers([
            HeaderName::from_static("acp-connection-id"),
            HeaderName::from_static("acp-session-id"),
        ])
}

/// Builds the complete Axum application with shared HTTP layers.
pub(crate) fn build_app(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
    terminal_router: Arc<AcpClientTerminalRouter>,
    options: &HttpServerOptions,
) -> Router {
    // Keep all transport and static routes in `routers`, while app-level
    // middleware remains here so cross-cutting HTTP behavior is centralized.
    build_router(kernel, fs_router, terminal_router, options)
        .layer(cors_layer())
}

/// Runs the ACP HTTP server until the listener fails or the process exits.
pub async fn run_with_routers(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
    terminal_router: Arc<AcpClientTerminalRouter>,
    options: HttpServerOptions,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(options.bind).await?;
    let addr = listener.local_addr()?;
    let options = HttpServerOptions {
        bind: addr,
        ..options
    };
    let app = build_app(kernel, fs_router, terminal_router, &options);
    eprintln!("clawcode web: http://{addr}");
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the HTTP server defaults stay local-only.
    #[test]
    fn http_options_default_to_localhost_and_acp_path() {
        let options = HttpServerOptions::default();

        assert_eq!(options.bind.ip().to_string(), "127.0.0.1");
        assert_eq!(options.bind.port(), 0);
        assert_eq!(options.acp_path, "/acp");
    }

    /// Verifies the app-level CORS layer can be constructed.
    #[test]
    fn app_cors_layer_builds() {
        let _layer = cors_layer();
    }
}
