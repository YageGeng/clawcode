use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use acp::{AcpServerFactory, HttpTransportOptions};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use protocol::ProductIdentity;
use serde::Serialize;
use tower_http::services::{ServeDir, ServeFile};

use crate::{Application, ApplicationError};

/// Product labels supplied to the browser without duplicated name literals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductInfo {
    /// Human-readable product title.
    pub name: String,
    /// Stable machine-readable product slug.
    pub slug: String,
}

/// Read-only active model information displayed by the first WebUI version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveModelInfo {
    /// Configured provider/model identifier.
    pub id: String,
    /// Configured model display name or the model id fallback.
    pub display_name: String,
}

/// Immutable same-origin startup data required before opening ACP WebSocket.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct UiBootstrapResponse {
    /// Centralized product identity.
    pub product: ProductInfo,
    /// Same-origin ACP WebSocket route.
    pub acp_path: String,
    /// Absolute service working directory used by new-session defaults.
    pub default_cwd: String,
    /// Active immutable model selection.
    pub active_model: ActiveModelInfo,
}

/// Same-origin Web server routes and static asset location.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct WebServerOptions {
    /// ACP HTTP, SSE, and WebSocket path.
    pub acp_path: String,
    /// Vite production output directory.
    pub web_root: PathBuf,
    /// Exact loopback browser origin accepted by the official WebSocket transport.
    pub browser_origin: String,
    /// Whether the official ACP transport exposes `/health`.
    pub health_endpoint: bool,
    /// Maximum number of queued Session updates emitted in one ACP batch.
    pub max_batch_size: NonZeroUsize,
}

/// Validated loopback-only listen address for the local single-user WebUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalBindAddress(SocketAddr);

impl LocalBindAddress {
    /// Returns the validated socket address for Tokio binding.
    #[must_use]
    pub const fn into_inner(self) -> SocketAddr {
        self.0
    }

    /// Returns the exact HTTP origin used by same-origin browser WebSocket handshakes.
    #[must_use]
    pub fn http_origin(self) -> String {
        format!("http://{}", self.0)
    }
}

impl TryFrom<SocketAddr> for LocalBindAddress {
    type Error = ApplicationError;

    /// Rejects wildcard, LAN, and public addresses in the unauthenticated first version.
    fn try_from(address: SocketAddr) -> Result<Self, Self::Error> {
        if !address.ip().is_loopback() {
            return Err(ApplicationError::NonLoopbackBind(address));
        }
        Ok(Self(address))
    }
}

impl Application {
    /// Combines bootstrap, official ACP transports, and SPA fallback on one origin.
    pub fn http_router(
        &self,
        options: WebServerOptions,
    ) -> Result<Router, ApplicationError> {
        let bootstrap = self.ui_bootstrap.clone();
        let router = AcpServerFactory::http_router(
            Arc::clone(&self.kernel),
            Arc::clone(&self.id_generator),
            options.max_batch_size,
            HttpTransportOptions {
                path: options.acp_path,
                allowed_origins: vec![options.browser_origin],
                health_endpoint: options.health_endpoint,
            },
        )?
        .route(
            ProductIdentity::UI_BOOTSTRAP_PATH,
            get(move || {
                let bootstrap = bootstrap.clone();
                async move { Json(bootstrap) }
            }),
        );
        let index = options.web_root.join("index.html");
        if index.is_file() {
            return Ok(router.fallback_service(
                ServeDir::new(options.web_root).fallback(ServeFile::new(index)),
            ));
        }

        Ok(router.fallback(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "WebUI assets are unavailable; run npm run build"
                })),
            )
        }))
    }
}
