//! Axum routers for ACP transport and embedded web assets.

use std::sync::Arc;

use axum::{
    Router,
    http::header,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::agent::ClawcodeAgent;
use crate::backend::fs::AcpClientFsRouter;
use crate::backend::terminal::AcpClientTerminalRouter;
use agent_client_protocol_http::{AcpHttpServer, CorsOptions, ServerOptions};
use protocol::AgentKernel;

use super::app::HttpServerOptions;

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const STYLES_CSS: &str = include_str!("../../web/styles.css");

/// Builds the static web router mounted next to the ACP endpoint.
pub(crate) fn web_router() -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/app.js", get(app_js))
        .route("/styles.css", get(styles_css))
}

/// Builds the official ACP HTTP server options from clawcode options.
pub(crate) fn acp_options(options: &HttpServerOptions) -> ServerOptions {
    ServerOptions {
        path: options.acp_path.clone(),
        // The official transport still performs WebSocket Origin validation
        // internally, so let it accept browser origins while app.rs owns the
        // Axum CORS layer applied to the combined application.
        cors: CorsOptions::allow_any_origin(),
        health_endpoint: true,
    }
}

/// Builds the ACP transport router for the clawcode agent.
pub(crate) fn acp_router(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
    terminal_router: Arc<AcpClientTerminalRouter>,
    options: &HttpServerOptions,
) -> Router {
    let acp_options = acp_options(options);
    AcpHttpServer::new(move || {
        ClawcodeAgent::with_routers(
            Arc::clone(&kernel),
            Arc::clone(&fs_router),
            Arc::clone(&terminal_router),
        )
    })
    .with_options(acp_options)
    .into_router()
}

/// Builds the combined router for web assets and the ACP endpoint.
pub(crate) fn build_router(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
    terminal_router: Arc<AcpClientTerminalRouter>,
    options: &HttpServerOptions,
) -> Router {
    web_router().merge(acp_router(kernel, fs_router, terminal_router, options))
}

/// Returns the embedded web chat shell.
async fn index_html() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// Returns the embedded web chat JavaScript.
async fn app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

/// Returns the embedded web chat stylesheet.
async fn styles_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    /// Verifies embedded web assets contain the required ACP entry points.
    #[test]
    fn embedded_web_assets_reference_required_acp_methods() {
        assert!(INDEX_HTML.contains("app.js"));
        assert!(INDEX_HTML.contains("id=\"sessions\""));
        assert!(INDEX_HTML.contains("id=\"modelSelect\""));
        assert!(INDEX_HTML.contains("id=\"modelSelectButton\""));
        assert!(INDEX_HTML.contains("id=\"modelOptionsMenu\""));
        assert!(INDEX_HTML.contains("id=\"transcript\""));
        assert!(INDEX_HTML.contains("id=\"permissionModal\""));
        assert!(INDEX_HTML.contains("id=\"ctxMeter\""));
        assert!(INDEX_HTML.contains("id=\"ctxTooltip\""));
        assert!(INDEX_HTML.contains("id=\"ctxMeterValue\">0"));
        assert!(INDEX_HTML.contains("class=\"composer-shell\""));
        assert!(INDEX_HTML.contains("class=\"composer-toolbar\""));
        assert!(INDEX_HTML.contains("id=\"sendCtxButton\""));
        assert!(INDEX_HTML.contains("class=\"send-button\""));
        assert!(INDEX_HTML.contains("class=\"send-button-arrow\""));
        assert!(!INDEX_HTML.contains("Add context"));
        assert!(!INDEX_HTML.contains("Full access"));
        assert!(INDEX_HTML.contains("marked"));
        assert!(INDEX_HTML.contains("DOMPurify"));
        assert!(INDEX_HTML.contains("highlight.js"));
        // Security-sensitive web dependencies must be constrained because this
        // page can drive the local ACP agent over WebSocket.
        assert!(INDEX_HTML.contains("http-equiv=\"Content-Security-Policy\""));
        assert!(INDEX_HTML.contains("default-src 'self'"));
        assert!(
            INDEX_HTML.contains("script-src 'self' https://cdn.jsdelivr.net")
        );
        assert!(
            INDEX_HTML.contains("style-src 'self' https://cdn.jsdelivr.net")
        );
        assert!(INDEX_HTML.contains("connect-src 'self' ws: wss:"));
        assert!(INDEX_HTML.contains("sha384-eFTL69TLRZTkNfYZOLM+G04821K1qZao/4QLJbet1pP4tcF+fdXq/9CdqAbWRl/L"));
        assert!(INDEX_HTML.contains("sha384-/TQbtLCAerC3jgaim+N78RZSDYV7ryeoBCVqTuzRrFec2akfBkHS7ACQ3PQhvMVi"));
        assert!(INDEX_HTML.contains("sha384-+VfUPEb0PdtChMwmBcBmykRMDd+v6D/oFmB3rZM/puCMDYcIvF968OimRh4KQY9a"));
        assert!(INDEX_HTML.contains("sha384-wQX/HE4cHEfyVNknht8z4V6Sv7BRsHKI3XXO26tXc0424Y+zxIOWqRxKjHAKRUdr"));
        assert!(INDEX_HTML.contains("crossorigin=\"anonymous\""));
        assert!(APP_JS.contains("session/list"));
        assert!(APP_JS.contains("session/load"));
        assert!(APP_JS.contains("session/set_config_option"));
        assert!(APP_JS.contains("session/request_permission"));
        assert!(APP_JS.contains("currentThought"));
        assert!(APP_JS.contains("appendThoughtText"));
        assert!(APP_JS.contains("pendingPermissions"));
        assert!(APP_JS.contains("rejectPendingRequests"));
        assert!(APP_JS.contains("cancelPendingPermissions"));
        assert!(APP_JS.contains("params.sessionId !== state.currentSessionId"));
        assert!(APP_JS.contains("user_message_chunk"));
        assert!(APP_JS.contains("appendUserText"));
        assert!(APP_JS.contains("resetActiveTextStreams"));
        assert!(APP_JS.contains("available_commands_update"));
        assert!(APP_JS.contains("isMetadataOnlyToolUpdate"));
        assert!(APP_JS.contains("toolBlocks"));
        assert!(APP_JS.contains("renderToolUpdate"));
        assert!(APP_JS.contains("ensureToolBlock"));
        assert!(APP_JS.contains("appendCollapsedToolSection"));
        assert!(APP_JS.contains("toolStatusKind"));
        assert!(APP_JS.contains("setMessageMarkdown"));
        assert!(APP_JS.contains("renderMarkdown"));
        assert!(APP_JS.contains("renderToolJson"));
        assert!(APP_JS.contains("language-json"));
        assert!(APP_JS.contains("hljs.highlightElement"));
        assert!(APP_JS.contains("renderUsageUpdate"));
        assert!(APP_JS.contains("formatContextUsage"));
        assert!(APP_JS.contains("formatTokenUsage"));
        assert!(APP_JS.contains("cacheHitRatePercent"));
        assert!(APP_JS.contains("latestProviderUsage"));
        assert!(APP_JS.contains("updateProviderUsage"));
        assert!(APP_JS.contains("ctxMeter.title"));
        assert!(APP_JS.contains("renderModelOptions"));
        assert!(APP_JS.contains("toggleModelMenu"));
        assert!(APP_JS.contains("closeModelMenu"));
        assert!(APP_JS.contains("chooseModelOption"));
        assert!(APP_JS.contains("--ctx-percent"));
        assert!(STYLES_CSS.contains("height: 100dvh"));
        assert!(STYLES_CSS.contains("overflow: hidden"));
        assert!(STYLES_CSS.contains("overscroll-behavior: contain"));
        assert!(STYLES_CSS.contains(".tool-section"));
        assert!(STYLES_CSS.contains(".tool-section summary"));
        assert!(STYLES_CSS.contains(".tool-status-dot"));
        assert!(STYLES_CSS.contains(".tool-status-success"));
        assert!(STYLES_CSS.contains(".tool-status-error"));
        assert!(STYLES_CSS.contains(".markdown-body"));
        assert!(STYLES_CSS.contains(".markdown-body pre"));
        assert!(STYLES_CSS.contains(".tool-code"));
        assert!(STYLES_CSS.contains(".ctx-meter"));
        assert!(STYLES_CSS.contains(".ctx-ring"));
        assert!(STYLES_CSS.contains(".usage-tooltip"));
        assert!(STYLES_CSS.contains("bottom: calc(100% + 10px)"));
        assert!(STYLES_CSS.contains(".ctx-meter:hover .usage-tooltip"));
        assert!(STYLES_CSS.contains(".composer-shell"));
        assert!(STYLES_CSS.contains(".composer-toolbar"));
        assert!(STYLES_CSS.contains(".send-ctx-button"));
        assert!(STYLES_CSS.contains(".send-button"));
        assert!(STYLES_CSS.contains(".composer-ctx-meter .ctx-ring"));
        assert!(STYLES_CSS.contains("appearance: none"));
        assert!(STYLES_CSS.contains("background-image"));
        assert!(STYLES_CSS.contains(".model-native-select"));
        assert!(STYLES_CSS.contains(".model-select-button"));
        assert!(STYLES_CSS.contains(".model-options-menu"));
        assert!(STYLES_CSS.contains(".model-option"));
        assert!(STYLES_CSS.contains(".composer-model-picker::after"));
        assert!(STYLES_CSS.contains(".send-button-arrow"));
        assert!(!STYLES_CSS.contains(".composer-icon-button"));
        assert!(!STYLES_CSS.contains(".context-access-button"));
    }

    /// Verifies the static web router can be constructed for HTTP mode.
    #[test]
    fn web_router_builds() {
        let _router = web_router();
    }

    /// Verifies ACP HTTP options preserve the path and delegate CORS to layers.
    #[test]
    fn acp_server_options_delegate_cors_to_layers() {
        let options = HttpServerOptions {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            acp_path: "/custom-acp".to_string(),
        };

        let server_options = acp_options(&options);

        assert_eq!(server_options.path, "/custom-acp");
        assert!(server_options.health_endpoint);
        assert_eq!(server_options.cors, CorsOptions::AllowAnyOrigin);
    }
}
