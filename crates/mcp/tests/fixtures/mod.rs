//! Test fixture MCP servers for integration testing.

use rmcp::ServiceExt;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

// ---------------------------------------------------------------------------
// EchoServer — one tool that echoes input back
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct EchoParams {
    message: String,
}

pub struct EchoServer;

#[tool_router]
impl EchoServer {
    #[tool(description = "Echo a message back to the caller")]
    fn echo(&self, Parameters(params): Parameters<EchoParams>) -> String {
        params.message
    }
}

#[tool_handler]
impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

// ---------------------------------------------------------------------------
// CalcServer — two arithmetic tools
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SumParams {
    a: i32,
    b: i32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct MulParams {
    x: f64,
    y: f64,
}

pub struct CalcServer;

#[tool_router]
impl CalcServer {
    #[tool(description = "Add two integers")]
    fn add(&self, Parameters(params): Parameters<SumParams>) -> String {
        (params.a + params.b).to_string()
    }

    #[tool(description = "Multiply two floats")]
    fn multiply(&self, Parameters(params): Parameters<MulParams>) -> String {
        (params.x * params.y).to_string()
    }
}

#[tool_handler]
impl ServerHandler for CalcServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

// ---------------------------------------------------------------------------
// EmptyServer — no tools, for testing empty catalog
// ---------------------------------------------------------------------------

pub struct EmptyServer;

impl ServerHandler for EmptyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::default())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Handle that keeps a test MCP server alive.
pub struct ServerGuard {
    _handle: tokio::task::JoinHandle<()>,
}

/// Spawn an MCP server on a duplex channel, returning the client-side
/// transport and a guard that keeps the server alive.
pub fn spawn_server<S>(server: S) -> (tokio::io::DuplexStream, ServerGuard)
where
    S: ServerHandler + Send + 'static,
{
    let (server_tx, client_rx) = tokio::io::duplex(8192);
    let handle = tokio::spawn(async move {
        let running = server
            .serve(server_tx)
            .await
            .expect("server failed to start");
        let _ = running.waiting().await.ok();
    });
    (client_rx, ServerGuard { _handle: handle })
}
