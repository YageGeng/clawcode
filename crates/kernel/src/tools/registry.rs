use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::tools::Tool;

#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    /// Registers a concrete tool instance.
    pub async fn register<T>(&self, tool: T)
    where
        T: Tool + 'static,
    {
        self.register_arc(Arc::new(tool)).await;
    }

    /// Registers a shared tool instance.
    pub async fn register_arc(&self, tool: Arc<dyn Tool>) {
        self.tools
            .write()
            .await
            .insert(tool.name().to_string(), tool);
    }

    /// Returns a tool by name.
    pub async fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let normalized_name = normalize_tool_name_for_lookup(name);
        let tools = self.tools.read().await;

        // Keep strict exact-match behavior first so registered aliases can still be
        // looked up directly by their canonical names.
        if let Some(tool) = tools.get(name) {
            return Some(tool.clone());
        }

        // If the provider returned a normalized tool name (for example `fs_read_text_file`)
        // map it back to the canonical tool registration (for example `fs/read_text_file`).
        for tool in tools.values() {
            if normalize_tool_name_for_lookup(tool.name()) == normalized_name {
                return Some(tool.clone());
            }
        }

        None
    }

    /// Returns sorted tool definitions that can be exposed to the model.
    pub async fn definitions(&self) -> Vec<llm::completion::ToolDefinition> {
        let mut definitions = self
            .tools
            .read()
            .await
            .values()
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        definitions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        definitions
    }
}

/// Applies the same sanitization used by the responses API for tool-name aliases.
fn normalize_tool_name_for_lookup(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "tool".to_string()
    } else {
        sanitized
    }
}
