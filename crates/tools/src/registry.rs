use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::warn;

use crate::handler::ToolHandler;
use crate::router::ToolRouter;
use crate::spec::{ConfiguredToolSpec, ToolSpec};

/// Stores the concrete handlers available to a router.
#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn ToolHandler>>>,
    /// Maps normalized tool names to handlers for O(1) alias resolution.
    aliases: RwLock<HashMap<String, Arc<dyn ToolHandler>>>,
}

impl ToolRegistry {
    /// Registers a concrete tool instance.
    pub async fn register<T>(&self, tool: T)
    where
        T: ToolHandler + 'static,
    {
        self.register_arc(Arc::new(tool)).await;
    }

    /// Registers a shared tool instance and its normalized-name alias.
    pub async fn register_arc(&self, tool: Arc<dyn ToolHandler>) {
        let name = tool.name().to_string();
        let normalized = normalize_tool_name_for_lookup(&name);
        self.aliases
            .write()
            .await
            .insert(normalized, Arc::clone(&tool));
        self.tools.write().await.insert(name, tool);
    }

    /// Looks up a tool by its stable name or sanitized alias.
    pub async fn get(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        let tools = self.tools.read().await;
        if let Some(tool) = tools.get(name) {
            return Some(tool.clone());
        }
        // Release tools read lock before acquiring aliases lock.
        drop(tools);

        let normalized = normalize_tool_name_for_lookup(name);
        self.aliases.read().await.get(&normalized).cloned()
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

/// Builds a paired set of visible specs and executable handlers.
pub struct ToolRegistryBuilder {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    specs: Vec<ConfiguredToolSpec>,
}

impl Default for ToolRegistryBuilder {
    /// Builds the default empty router builder.
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistryBuilder {
    /// Creates an empty builder for router assembly.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            specs: Vec::new(),
        }
    }

    /// Pushes a visible tool spec while also registering the same handler for dispatch.
    pub fn push_handler_spec(&mut self, handler: Arc<dyn ToolHandler>) {
        self.push_configured_handler_spec(handler, false);
    }

    /// Pushes a visible tool spec with explicit parallel support metadata.
    pub fn push_configured_handler_spec(
        &mut self,
        handler: Arc<dyn ToolHandler>,
        supports_parallel_tool_calls: bool,
    ) {
        let visible_when = handler.visible_when();
        self.specs.push(ConfiguredToolSpec::new(
            ToolSpec::function_with_prompt(handler.definition(), handler.prompt_metadata()),
            supports_parallel_tool_calls,
            visible_when,
        ));
        self.register_handler(handler.name(), handler);
    }

    /// Pushes a preconfigured visible spec without rewriting its visibility predicate.
    pub fn push_configured_spec(&mut self, spec: ConfiguredToolSpec) {
        self.specs.push(spec);
    }

    /// Registers a visible tool spec without touching handler registration.
    pub fn push_spec(&mut self, spec: ToolSpec, supports_parallel_tool_calls: bool) {
        self.specs.push(ConfiguredToolSpec::new(
            spec,
            supports_parallel_tool_calls,
            None,
        ));
    }

    /// Registers a handler under an explicit dispatch name.
    pub fn register_handler(&mut self, name: &str, handler: Arc<dyn ToolHandler>) {
        if self.handlers.insert(name.to_string(), handler).is_some() {
            warn!("overwriting handler for tool {name}");
        }
    }

    /// Finalizes the builder into a router that owns visible specs plus dispatch handlers.
    pub fn build_router(self) -> ToolRouter {
        let mut aliases = HashMap::new();
        for (name, handler) in &self.handlers {
            let normalized = normalize_tool_name_for_lookup(name);
            aliases.insert(normalized, Arc::clone(handler));
        }
        let registry = Arc::new(ToolRegistry {
            tools: RwLock::new(self.handlers),
            aliases: RwLock::new(aliases),
        });
        ToolRouter::new(registry, self.specs)
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
