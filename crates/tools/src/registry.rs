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
}

impl ToolRegistry {
    /// Registers a concrete tool instance.
    pub async fn register<T>(&self, tool: T)
    where
        T: ToolHandler + 'static,
    {
        self.register_arc(Arc::new(tool)).await;
    }

    /// Registers a shared tool instance.
    pub async fn register_arc(&self, tool: Arc<dyn ToolHandler>) {
        self.tools
            .write()
            .await
            .insert(tool.name().to_string(), tool);
    }

    /// Looks up a tool by its stable name or sanitized alias.
    pub async fn get(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        let normalized_name = normalize_tool_name_for_lookup(name);
        let tools = self.tools.read().await;

        if let Some(tool) = tools.get(name) {
            return Some(tool.clone());
        }

        // Normalize names so `fs_read_text_file` still resolves to `fs/read_text_file`.
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
        self.push_configured_handler_spec(handler, /*supports_parallel_tool_calls*/ false);
    }

    /// Pushes a visible tool spec with explicit parallel support metadata.
    pub fn push_configured_handler_spec(
        &mut self,
        handler: Arc<dyn ToolHandler>,
        supports_parallel_tool_calls: bool,
    ) {
        self.push_spec(
            ToolSpec::function_with_prompt(handler.definition(), handler.prompt_metadata()),
            supports_parallel_tool_calls,
        );
        self.register_handler(handler.name(), handler);
    }

    /// Registers a visible tool spec without touching handler registration.
    pub fn push_spec(&mut self, spec: ToolSpec, supports_parallel_tool_calls: bool) {
        self.specs
            .push(ConfiguredToolSpec::new(spec, supports_parallel_tool_calls));
    }

    /// Registers a handler under an explicit dispatch name.
    pub fn register_handler(&mut self, name: impl Into<String>, handler: Arc<dyn ToolHandler>) {
        let name = name.into();
        if self.handlers.insert(name.clone(), handler).is_some() {
            warn!("overwriting handler for tool {name}");
        }
    }

    /// Finalizes the builder into a router that owns visible specs plus dispatch handlers.
    pub fn build_router(self) -> ToolRouter {
        let registry = Arc::new(ToolRegistry {
            tools: RwLock::new(self.handlers),
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
