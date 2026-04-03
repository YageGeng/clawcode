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
        self.tools.read().await.get(name).cloned()
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
