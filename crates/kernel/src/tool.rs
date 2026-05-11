//! Tool registration and execution for agent turns.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

/// A tool that can be invoked by the LLM during a turn.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name exposed to the model.
    fn name(&self) -> &str;

    /// Human-readable description sent to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with the given JSON arguments.
    /// Returns the output string on success, or an error message on failure.
    async fn execute(&self, arguments: serde_json::Value, cwd: &Path) -> Result<String, String>;
}

/// Registry of available tools, keyed by tool name.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Build tool definitions for the LLM completion request.
    #[must_use]
    pub fn definitions(&self) -> Vec<protocol::ToolDefinition> {
        self.tools
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
        cwd: &Path,
    ) -> Result<String, String> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(arguments, cwd).await,
            None => Err(format!("unknown tool: {name}")),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Mock tools for testing ──

/// A mock tool that echoes its arguments — useful for testing the tool pipeline.
pub struct MockEchoTool {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
}

#[async_trait]
impl Tool for MockEchoTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to echo"
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        let msg = arguments["message"].as_str().unwrap_or("(no message)");
        Ok(format!("echo: {msg}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_register_and_definitions() {
        let mut reg = ToolRegistry::new();
        let tool = Arc::new(MockEchoTool {
            name: "echo".to_string(),
            description: "Echoes a message".to_string(),
        });
        reg.register(tool);
        let defs = reg.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn registry_execute_unknown_tool() {
        let reg = ToolRegistry::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(reg.execute("nonexistent", serde_json::json!({}), Path::new(".")));
        assert!(result.is_err());
    }
}
