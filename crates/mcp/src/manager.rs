//! `McpConnectionManager` — per-session MCP server lifecycle and tool aggregation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::client::ManagedClient;
use crate::{McpError, McpServerConfig};
use crate::{McpStartupStatus, McpToolInfo, normalize_tool_name};

/// Manages all MCP server connections for a session.
///
/// Constructed once with a set of configs, then `start_all` can be called
/// (typically from a background task) to begin connecting.  Internal state
/// is protected by `Arc<Mutex<>>` so the manager can be shared across tasks.
#[derive(typed_builder::TypedBuilder)]
pub struct McpConnectionManager {
    configs: Vec<McpServerConfig>,
    auth_dir: PathBuf,
    #[builder(default)]
    clients: Mutex<HashMap<String, ManagedClient>>,
    #[builder(default)]
    startup_status: Mutex<HashMap<String, McpStartupStatus>>,
}

impl McpConnectionManager {
    /// Create a new manager with the given server configs.
    ///
    /// Servers are NOT started yet — call [`start_all`](Self::start_all) or
    /// [`spawn_background`](Self::spawn_background) to initiate connections.
    pub fn new(configs: Vec<McpServerConfig>, auth_dir: PathBuf) -> Self {
        Self::builder().configs(configs).auth_dir(auth_dir).build()
    }

    /// Start all enabled MCP servers, populating the internal client map.
    ///
    /// Idempotent — if servers are already connected, this is a no-op.
    pub async fn start_all(&self) {
        self.start_all_with(|cfg, dir| {
            Box::pin(async move { ManagedClient::connect(&cfg, &dir).await })
        })
        .await;
    }

    /// Start all enabled servers with an injectable connector for in-memory tests.
    #[cfg(test)]
    pub(crate) async fn start_all_with_connector<T, F, E, A>(&self, stdio_connector: F)
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
        F: Fn(tokio::process::Command) -> Result<T, McpError> + Send + Sync + Clone + 'static,
    {
        self.start_all_with(move |cfg, dir| {
            let connector = stdio_connector.clone();
            Box::pin(
                async move { ManagedClient::connect_with_connector(&cfg, &dir, connector).await },
            )
        })
        .await;
    }

    /// Start all enabled servers using the supplied connection strategy.
    async fn start_all_with<F>(&self, connect: F)
    where
        F: Fn(
                McpServerConfig,
                PathBuf,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ManagedClient, McpError>> + Send>,
            > + Send
            + Sync
            + Clone
            + 'static,
    {
        {
            let clients = self
                .clients
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !clients.is_empty() {
                return;
            }
        }

        // Collect connection handles without holding any lock across awaits.
        let mut handles = Vec::new();
        for config in &self.configs {
            if !config.enabled {
                continue;
            }
            let cfg = config.clone();
            let dir = self.auth_dir.clone();
            let connect = connect.clone();
            handles.push(tokio::spawn(async move {
                (cfg.name.clone(), connect(cfg, dir).await)
            }));
        }

        // Await all results.
        let mut results: Vec<(String, Result<ManagedClient, McpError>)> = Vec::new();
        for handle in handles {
            let (name, result) = handle.await.unwrap_or_else(|e| {
                (
                    String::new(),
                    Err(McpError::Startup {
                        server: "unknown".into(),
                        reason: format!("join error: {e}"),
                    }),
                )
            });
            results.push((name, result));
        }

        // Populate internal maps under locks.
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut status = self
            .startup_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (name, result) in results {
            match result {
                Ok(client) => {
                    status.insert(name.clone(), McpStartupStatus::Ready);
                    clients.insert(name, client);
                }
                Err(e) => {
                    status.insert(
                        name.clone(),
                        McpStartupStatus::Failed {
                            reason: e.to_string(),
                        },
                    );
                }
            }
        }
    }

    /// Start all servers in a background task, returning a oneshot receiver
    /// that fires once initialization is complete.
    pub fn spawn_background(self: &Arc<Self>) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mgr = Arc::clone(self);

        tokio::spawn(async move {
            mgr.start_all().await;

            let status = mgr
                .startup_status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (name, mcp_status) in status.iter() {
                match mcp_status {
                    McpStartupStatus::Ready => tracing::info!(server = %name, "MCP connected"),
                    McpStartupStatus::Failed { reason } => {
                        tracing::warn!(server = %name, %reason, "MCP failed to start")
                    }
                }
            }

            let _ = tx.send(());
        });
        rx
    }

    /// Per-server startup outcomes.
    pub fn startup_status(&self) -> HashMap<String, McpStartupStatus> {
        self.startup_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Collect tools from all connected servers with `mcp__<server>__<tool>` naming.
    pub fn list_all_tools(&self) -> Vec<McpToolInfo> {
        let clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut tools = Vec::new();
        for client in clients.values() {
            for t in &client.tools {
                let mut prefixed = t.clone();
                prefixed.callable_name = format!(
                    "mcp__{}__{}",
                    normalize_tool_name(&client.server_name),
                    normalize_tool_name(&t.raw_name),
                );
                prefixed.server_name = client.server_name.clone();
                tools.push(prefixed);
            }
        }
        tools
    }

    /// Call a tool on a named server. Errors returned as `Err(String)`, never panics.
    pub async fn call_tool(
        &self,
        server: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, String> {
        use rmcp::model::RawContent;
        use std::time::Duration;
        use tokio::time::timeout;

        // Clone the service handle and timeout under the lock, then release.
        let (service, tool_timeout_secs) = {
            let clients = self
                .clients
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let client = clients.get(server).ok_or_else(|| {
                let status = self
                    .startup_status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match status.get(server) {
                    Some(McpStartupStatus::Failed { reason }) => {
                        format!("MCP server '{server}' failed to start: {reason}")
                    }
                    _ => format!("MCP server '{server}' not found"),
                }
            })?;
            (client.service.clone(), client.tool_timeout_secs)
        };

        let svc = service.ok_or_else(|| format!("MCP server '{server}' is not connected"))?;

        let obj_args = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => Some(serde_json::Map::from_iter([("value".to_string(), other)])),
        };

        let mut params = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
        if let Some(a) = obj_args {
            params = params.with_arguments(a);
        }

        let call_future = async { svc.lock().await.call_tool(params).await };

        let result = timeout(Duration::from_secs(tool_timeout_secs), call_future)
            .await
            .map_err(|_| {
                format!("tool '{tool_name}' on '{server}' timed out after {tool_timeout_secs}s",)
            })?
            .map_err(|e| format!("tool call failed on '{server}': {e}"))?;

        let structured_content = result.structured_content.clone();
        let text = result
            .content
            .into_iter()
            .filter_map(|c| match c.raw {
                RawContent::Text(t) => Some(t.text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let output = if text.is_empty() {
            // MCP allows structured-only results, so surface them instead of returning empty output.
            structured_content
                .as_ref()
                .map_or_else(String::new, serde_json::Value::to_string)
        } else {
            text
        };

        if result.is_error.unwrap_or(false) {
            Err(output)
        } else {
            Ok(output)
        }
    }

    /// Shut down all connections.
    pub async fn shutdown(&self) {
        self.clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
    use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
    use serde_json::json;

    use super::*;
    use crate::McpTransportConfig;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct EchoParams {
        message: String,
    }

    struct EchoServer;

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

    #[derive(Clone)]
    struct ErrorServer;

    #[tool_router]
    impl ErrorServer {
        #[tool(description = "Always returns a tool-level error")]
        fn fail(&self) -> CallToolResult {
            CallToolResult::error(vec![Content::text("boom")])
        }
    }

    #[tool_handler]
    impl ServerHandler for ErrorServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }
    }

    /// Spawn an in-memory MCP server and return the client side stream.
    fn spawn_server<S>(server: S) -> tokio::io::DuplexStream
    where
        S: ServerHandler + Send + 'static,
    {
        let (server_tx, client_rx) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let running = server.serve(server_tx).await.expect("server should start");
            let _ = running.waiting().await;
        });
        client_rx
    }

    /// Create a stdio MCP config whose command is intercepted by the test connector.
    fn stdio_config(name: &str) -> McpServerConfig {
        McpServerConfig::builder()
            .name(name.to_string())
            .transport(McpTransportConfig::Stdio {
                command: name.to_string(),
                args: Vec::new(),
                env: HashMap::new(),
            })
            .build()
    }

    #[tokio::test]
    async fn manager_lists_prefixed_tools_after_start() {
        let manager = McpConnectionManager::new(
            vec![stdio_config("echo/server")],
            tempfile::tempdir().unwrap().path().to_path_buf(),
        );

        manager
            .start_all_with_connector(|_cmd| Ok(spawn_server(EchoServer)))
            .await;

        let tools = manager.list_all_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].callable_name, "mcp__echo_server__echo");
    }

    #[tokio::test]
    async fn manager_call_tool_returns_tool_level_errors_as_err() {
        let manager = McpConnectionManager::new(
            vec![stdio_config("errors")],
            tempfile::tempdir().unwrap().path().to_path_buf(),
        );

        manager
            .start_all_with_connector(|_cmd| Ok(spawn_server(ErrorServer)))
            .await;

        let result = manager.call_tool("errors", "fail", json!({})).await;
        assert_eq!(result.expect_err("tool-level error should be Err"), "boom");
    }

    #[tokio::test]
    async fn manager_records_failed_startup_status() {
        let manager = McpConnectionManager::new(
            vec![stdio_config("missing")],
            tempfile::tempdir().unwrap().path().to_path_buf(),
        );
        let error_message = Arc::new(Mutex::new(String::from("spawn denied")));

        manager
            .start_all_with_connector({
                let error_message = Arc::clone(&error_message);
                move |_cmd| -> Result<tokio::io::DuplexStream, McpError> {
                    Err(McpError::Startup {
                        server: "missing".to_string(),
                        reason: error_message.lock().unwrap().clone(),
                    })
                }
            })
            .await;

        let status = manager.startup_status();
        assert!(matches!(
            status.get("missing"),
            Some(McpStartupStatus::Failed { reason }) if reason.contains("spawn denied")
        ));
    }
}
