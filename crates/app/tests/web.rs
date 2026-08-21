use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use app::{
    ActiveModelInfo, Application, LocalBindAddress, ProductInfo,
    UiBootstrapResponse, WebServerOptions,
};
use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use extension::StaticExtensionFactory;
use futures::stream;
use kernel::{KernelFactory, Model, ModelError, ModelFactory, ModelStream};
use prompt::FilesystemPromptFactory;
use protocol::{
    IdGenerator, IdKind, ModelProfile, ModelRequest, ProductIdentity,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;
use tower::ServiceExt;

/// Model implementation that is never called by route-only integration tests.
struct UnusedModel {
    profile: ModelProfile,
}

#[async_trait]
impl Model for UnusedModel {
    /// Returns the deterministic profile required by the production model contract.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms that the route-only fixture has no external readiness dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Returns an empty stream if a route unexpectedly reaches the model.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        Ok(Box::pin(stream::empty()))
    }
}

/// Returns the shared unused model from the factory boundary.
struct StaticModelFactory;

impl ModelFactory for StaticModelFactory {
    /// Creates the route-test model without provider configuration.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(UnusedModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("unused".to_string())
                .display_name("Unused route model".to_string())
                .context_tokens(128_000)
                .max_output_tokens(8_000)
                .build(),
        }))
    }
}

/// Supplies unique readable identifiers if an ACP route creates a session.
struct RouteIds;

impl IdGenerator for RouteIds {
    /// Generates a stable route-test identifier for the requested kind.
    fn next(&self, kind: IdKind) -> String {
        format!("{}-route", kind.prefix())
    }
}

/// Owns a cloned router and optional temporary static root.
struct WebFixture {
    router: Router,
    _web_root: Option<tempfile::TempDir>,
}

impl WebFixture {
    /// Creates one application router with a minimal existing SPA entry file.
    fn built() -> Self {
        let root = tempfile::tempdir().expect("web root");
        std::fs::write(root.path().join("index.html"), "<!doctype html>")
            .expect("write index");
        Self::new(root.path().to_path_buf(), Some(root))
    }

    /// Creates one application router pointing at an absent SPA directory.
    fn missing_dist() -> Self {
        let root = tempfile::tempdir().expect("parent root");
        Self::new(root.path().join("missing"), Some(root))
    }

    /// Composes a route-only application with deterministic local dependencies.
    fn new(
        web_root: PathBuf,
        retained_root: Option<tempfile::TempDir>,
    ) -> Self {
        let store_root = tempfile::tempdir().expect("store root");
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let kernel = KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                store_root.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                store_root.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(RouteIds))
            .build()
            .build()
            .expect("kernel");
        let application = Application {
            kernel: Arc::new(kernel),
            id_generator: Arc::new(RouteIds),
            ui_bootstrap: UiBootstrapResponse::builder()
                .product(ProductInfo {
                    name: ProductIdentity::NAME.to_string(),
                    slug: ProductIdentity::SLUG.to_string(),
                })
                .acp_path(ProductIdentity::DEFAULT_ACP_PATH.to_string())
                .default_cwd("/workspace".to_string())
                .active_model(ActiveModelInfo {
                    id: "provider/model".to_string(),
                    display_name: "Model".to_string(),
                })
                .build(),
        };
        let router = application
            .http_router(
                WebServerOptions::builder()
                    .acp_path(ProductIdentity::DEFAULT_ACP_PATH.to_string())
                    .web_root(web_root)
                    .browser_origin("http://127.0.0.1:3000".to_string())
                    .health_endpoint(true)
                    .max_batch_size(
                        NonZeroUsize::new(128).expect("positive batch size"),
                    )
                    .build(),
            )
            .expect("router");
        Self {
            router,
            _web_root: retained_root,
        }
    }

    /// Sends one same-origin GET request through the in-memory router.
    async fn get(&self, path: &str) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    /// Decodes one bounded JSON response body.
    async fn response_json(
        response: axum::response::Response,
    ) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("JSON response")
    }
}

/// Bootstrap returns product identity, ACP path, cwd, and configured model display data.
#[tokio::test]
async fn bootstrap_uses_product_identity_and_configured_model() {
    let fixture = WebFixture::built();
    let response = fixture.get(ProductIdentity::UI_BOOTSTRAP_PATH).await;
    let json = WebFixture::response_json(response).await;
    assert_eq!(json["product"]["name"], ProductIdentity::NAME);
    assert_eq!(json["product"]["slug"], ProductIdentity::SLUG);
    assert_eq!(json["acpPath"], ProductIdentity::DEFAULT_ACP_PATH);
    assert_eq!(json["activeModel"]["id"], "provider/model");
}

/// Missing frontend assets do not disable ACP transport health checks.
#[tokio::test]
async fn missing_web_root_keeps_acp_and_health_available() {
    let fixture = WebFixture::missing_dist();
    assert_eq!(
        fixture.get("/").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.get("/health").await.status(), StatusCode::OK);
}

/// First-version WebUI listening rejects wildcard and non-loopback addresses.
#[test]
fn webui_bind_rejects_non_loopback_addresses() {
    let public: SocketAddr = "0.0.0.0:3000".parse().expect("address");
    LocalBindAddress::try_from(public).expect_err("public bind");
    let loopback: SocketAddr = "127.0.0.1:3000".parse().expect("address");
    let bind = LocalBindAddress::try_from(loopback).expect("loopback bind");
    assert_eq!(bind.http_origin(), "http://127.0.0.1:3000");
}
