//! Agent tool registry and built-in tools.

pub mod backend;
pub mod builtin;
pub mod mcp;

use async_trait::async_trait;
use futures::stream::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

pub use backend::fs::{
    FsBackend, FsBackendError, FsReadRequest, FsReadResponse, FsWriteRequest,
    FsWriteResponse, LocalFsBackend,
};
pub use backend::terminal::{
    LocalTerminalBackend, RunningTerminal, TerminalBackend,
    TerminalBackendError, TerminalCreateParams, TerminalEnvVariable,
    TerminalExitResult, TerminalOutputSnapshot,
};
pub use protocol::ToolContext;

/// Consumes provider-emitted tool argument deltas before tool execution starts.
pub trait ToolArgumentsConsumer: Send {
    /// Consume one streamed argument delta and return display items for the frontend.
    fn consume_delta(
        &mut self,
        call_id: &str,
        delta: &str,
    ) -> Vec<protocol::ToolArgumentsStreamItem>;

    /// Flush pending display state after argument streaming completes.
    fn finish(
        &mut self,
        call_id: &str,
    ) -> Result<Vec<protocol::ToolArgumentsStreamItem>, String>;
}

/// A tool that can be invoked by the LLM during a turn.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name exposed to the model.
    fn name(&self) -> &str;

    /// Human-readable description sent to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Tool capability descriptor, for dispatch path selection.
    /// Default: not streaming-capable.
    fn capability(&self) -> protocol::ToolCapability {
        protocol::ToolCapability::default()
    }

    /// Create a consumer for streamed tool arguments, when this tool supports previews.
    fn arguments_consumer(&self) -> Option<Box<dyn ToolArgumentsConsumer>> {
        None
    }

    /// Whether this specific invocation requires user approval.
    /// Default: `true` (safe-by-default).
    fn needs_approval(
        &self,
        _arguments: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> bool {
        true
    }

    /// Execute the tool with the given JSON arguments and turn context.
    /// Returns the model-facing output string on success, or an error message on failure.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<String, String>;

    /// Execute the tool and return a stream of lifecycle/display items.
    ///
    /// The default implementation calls [`execute`] and wraps the result text in
    /// a single [`ToolStreamItem::Final`] event. Streaming-capable tools should
    /// override this to emit [`ToolStreamItem::Begin`]/[`ToolStreamItem::End`]
    /// lifecycle events and [`ToolStreamItem::Delta`] incremental updates.
    async fn execute_streaming(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<
        Pin<Box<dyn Stream<Item = protocol::ToolStreamItem> + Send>>,
        String,
    > {
        match self.execute(arguments, ctx).await {
            Ok(text) => {
                let item = protocol::ToolStreamItem::Final {
                    content: text.clone(),
                    is_error: false,
                };
                Ok(Box::pin(futures::stream::once(async move { item })))
            }
            Err(err) => {
                let item = protocol::ToolStreamItem::Final {
                    content: err.clone(),
                    is_error: true,
                };
                Ok(Box::pin(futures::stream::once(async move { item })))
            }
        }
    }
}

/// Registry of available tools, keyed by tool name.
///
/// Uses interior mutability via [`std::sync::Mutex`] so callers can
/// register tools through a shared `Arc<ToolRegistry>` reference.
pub struct ToolRegistry {
    tools: std::sync::Mutex<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Register a tool. Takes `&self` so it can be called through `Arc<ToolRegistry>`.
    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    /// Build tool definitions for the LLM completion request.
    #[must_use]
    pub fn definitions(&self) -> Vec<protocol::ToolDefinition> {
        let mut definitions: Vec<_> = self
            .tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|t| protocol::ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect();

        // Keep provider request bodies stable regardless of HashMap iteration order.
        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        definitions
    }

    /// Execute a tool call by name.
    pub async fn execute(
        &self,
        name: &str,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<String, String> {
        match self.get(name) {
            Some(tool) => tool.execute(arguments, ctx).await,
            None => Err(format!("unknown tool: {name}")),
        }
    }

    /// Execute a streaming tool call by name.
    pub async fn execute_streaming(
        &self,
        name: &str,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<
        Pin<Box<dyn Stream<Item = protocol::ToolStreamItem> + Send>>,
        String,
    > {
        match self.get(name) {
            Some(tool) => tool.execute_streaming(arguments, ctx).await,
            None => Err(format!("unknown tool: {name}")),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestTool {
        name: &'static str,
    }

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "test tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<String, String> {
            Ok("ok".to_string())
        }
    }

    /// Verifies that tools do not consume streamed arguments unless they opt in.
    #[test]
    fn tool_arguments_consumer_defaults_to_none() {
        let tool = TestTool {
            name: "no_arguments_consumer",
        };

        assert!(tool.arguments_consumer().is_none());
    }

    /// Verifies that completion request tool definitions have a stable order.
    #[test]
    fn definitions_are_sorted_by_tool_name() {
        let registry = ToolRegistry::new();
        for name in [
            "zeta", "alpha", "tool", "beta", "read", "edit", "write", "apply",
        ] {
            registry.register(Arc::new(TestTool { name }));
        }

        let names: Vec<_> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();

        assert_eq!(
            names,
            vec![
                "alpha", "apply", "beta", "edit", "read", "tool", "write",
                "zeta"
            ]
        );
    }
}
