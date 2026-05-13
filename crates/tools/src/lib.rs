//! Agent tool registry and built-in tools.

pub mod builtin;
pub mod mcp;

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

pub use protocol::ToolContext;

/// A tool that can be invoked by the LLM during a turn.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name exposed to the model.
    fn name(&self) -> &str;

    /// Human-readable description sent to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Whether this specific invocation requires user approval.
    /// Default: `true` (safe-by-default).
    fn needs_approval(&self, _arguments: &serde_json::Value, _ctx: &ToolContext) -> bool {
        true
    }

    /// Execute the tool with the given JSON arguments and turn context.
    /// Returns the output string on success, or an error message on failure.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<String, String>;
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
        self.tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|t| protocol::ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
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
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
