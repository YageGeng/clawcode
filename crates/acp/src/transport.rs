use std::num::NonZeroUsize;
use std::sync::Arc;

use agent_client_protocol::{ConnectTo, Stdio};
use agent_client_protocol_http::{AcpHttpServer, CorsOptions, ServerOptions};
use axum::Router;
use kernel::Kernel;
use protocol::{IdGenerator, ProductIdentity};

use crate::{AcpServerFactory, AcpTransportKind};

/// HTTP/SSE and WebSocket route configuration for browser and remote clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpTransportOptions {
    /// Shared ACP endpoint used for POST, SSE GET, and WebSocket GET upgrade.
    pub path: String,
    /// Explicit browser origins; an empty list keeps CORS disabled.
    pub allowed_origins: Vec<String>,
    /// Whether to expose the transport's `/health` endpoint.
    pub health_endpoint: bool,
}

impl Default for HttpTransportOptions {
    /// Uses `/acp`, disabled browser CORS, and an enabled health endpoint.
    fn default() -> Self {
        Self {
            path: ProductIdentity::DEFAULT_ACP_PATH.to_string(),
            allowed_origins: Vec::new(),
            health_endpoint: true,
        }
    }
}

/// Errors produced while constructing or serving ACP transports.
#[derive(Debug, thiserror::Error)]
pub enum AcpTransportError {
    /// An allowed browser origin was not a valid HTTP header value.
    #[error("invalid ACP browser origin: {0}")]
    InvalidOrigin(String),
    /// The ACP stdio connection terminated with a protocol error.
    #[error("ACP stdio transport failed: {0}")]
    Protocol(#[from] agent_client_protocol::Error),
}

impl AcpServerFactory {
    /// Runs one ACP v2 connection over process stdin/stdout JSON-RPC framing.
    pub async fn serve_stdio(&self) -> Result<(), AcpTransportError> {
        self.component(AcpTransportKind::Stdio)
            .connect_to(Stdio::new())
            .await?;
        Ok(())
    }

    /// Builds the official HTTP/SSE router with WebSocket upgrade on the same path.
    pub fn http_router(
        kernel: Arc<Kernel>,
        id_generator: Arc<dyn IdGenerator>,
        max_batch_size: NonZeroUsize,
        options: HttpTransportOptions,
    ) -> Result<Router, AcpTransportError> {
        let cors = if options.allowed_origins.is_empty() {
            CorsOptions::disabled()
        } else {
            CorsOptions::allow_origins(&options.allowed_origins).map_err(
                |error| AcpTransportError::InvalidOrigin(error.to_string()),
            )?
        };
        let server_options = ServerOptions {
            path: options.path,
            cors,
            health_endpoint: options.health_endpoint,
        };

        // Every HTTP/SSE or WebSocket connection inherits the immutable
        // application batch limit captured at router construction time.
        let factory = Arc::new(AcpServerFactory::new(
            kernel,
            id_generator,
            max_batch_size,
        ));

        Ok(AcpHttpServer::new(move || {
            factory.component(AcpTransportKind::Http)
        })
        .with_options(server_options)
        .into_router())
    }
}
