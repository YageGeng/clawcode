use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use tools::{AgentTool, ToolError, ToolRegistry};

/// Immutable available and active tool state captured for one Turn.
#[derive(Clone)]
pub(super) struct SessionToolSnapshot {
    builtins: ToolRegistry,
    extensions: ToolRegistry,
    mcp: ToolRegistry,
    active: BTreeSet<String>,
    disabled_preferences: BTreeSet<String>,
    revision: u64,
}

impl SessionToolSnapshot {
    /// Composes semantic partitions in their stable override order.
    fn available_registry(&self) -> ToolRegistry {
        let mut available = self.builtins.clone();
        available.overlay(&self.extensions);
        available.overlay(&self.mcp);
        available
    }

    /// Builds the model-facing registry selected by this immutable snapshot.
    pub(super) fn active_registry(&self) -> Result<ToolRegistry, ToolError> {
        self.available_registry()
            .subset(self.active.iter().map(String::as_str))
    }

    /// Returns active names in deterministic lexical order.
    pub(super) fn active_names(&self) -> Vec<String> {
        self.active.iter().cloned().collect()
    }

    /// Returns all available names in deterministic lexical order.
    pub(super) fn available_names(&self) -> Vec<String> {
        self.available_registry()
            .names()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Validates active names against this exact available registry.
    pub(super) fn validate_active(
        &self,
        names: &[String],
    ) -> Result<(), ToolError> {
        let available = self.available_registry();
        for name in names {
            if available.tool(name).is_none() {
                return Err(ToolError::NotFound(name.clone()));
            }
        }
        Ok(())
    }
}

/// Session-local tool state that publishes versioned immutable snapshots.
pub(super) struct SessionToolState {
    current: RwLock<Arc<SessionToolSnapshot>>,
}

impl SessionToolState {
    /// Creates one validated session tool state from restored active names.
    pub(super) fn new(
        builtins: ToolRegistry,
        extensions: ToolRegistry,
        mcp: ToolRegistry,
        active: Vec<String>,
    ) -> Result<Self, ToolError> {
        let mut snapshot = SessionToolSnapshot {
            builtins,
            extensions,
            mcp,
            active: active.into_iter().collect(),
            disabled_preferences: BTreeSet::new(),
            revision: 1,
        };
        snapshot.validate_active(&snapshot.active_names())?;
        snapshot.disabled_preferences = snapshot
            .available_names()
            .into_iter()
            .filter(|name| !snapshot.active.contains(name))
            .collect();
        Ok(Self {
            current: RwLock::new(Arc::new(snapshot)),
        })
    }

    /// Clones the current immutable snapshot and releases the state lock.
    pub(super) fn snapshot(
        &self,
    ) -> Result<Arc<SessionToolSnapshot>, ToolError> {
        self.current
            .read()
            .map(|snapshot| Arc::clone(&snapshot))
            .map_err(|_poison_error| ToolError::RegistryPoisoned)
    }

    /// Replaces active names after validating the current available registry.
    pub(super) fn set_active(
        &self,
        names: Vec<String>,
    ) -> Result<(), ToolError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| ToolError::RegistryPoisoned)?;
        current.validate_active(&names)?;
        let mut next = current.as_ref().clone();
        next.active = names.into_iter().collect();
        // Only visible names participate in this user change. Preferences for
        // temporarily unavailable dynamic Tools must survive reconnects.
        for name in next.available_names() {
            if next.active.contains(&name) {
                next.disabled_preferences.remove(&name);
            } else {
                next.disabled_preferences.insert(name);
            }
        }
        next.revision = next.revision.saturating_add(1);
        *current = Arc::new(next);
        Ok(())
    }

    /// Registers and activates one tool in the same published snapshot.
    pub(super) fn register(
        &self,
        tool: Arc<dyn AgentTool>,
    ) -> Result<(), ToolError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| ToolError::RegistryPoisoned)?;
        let mut next = current.as_ref().clone();
        let name = tool.definition().name;
        next.extensions.upsert(tool);
        if !next.disabled_preferences.contains(&name) {
            next.active.insert(name);
        }
        next.revision = next.revision.saturating_add(1);
        *current = Arc::new(next);
        Ok(())
    }

    /// Atomically replaces only the MCP partition while preserving local Tools and preferences.
    pub(super) fn replace_mcp_partition(
        &self,
        mcp: ToolRegistry,
    ) -> Result<(), ToolError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| ToolError::RegistryPoisoned)?;
        let mut next = current.as_ref().clone();
        next.mcp = mcp;
        next.active = next
            .available_names()
            .into_iter()
            .filter(|name| !next.disabled_preferences.contains(name))
            .collect();
        next.revision = next.revision.saturating_add(1);
        *current = Arc::new(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use protocol::{ToolCall, ToolDefinition, ToolResult};
    use tools::{AgentTool, ToolExecutionContext, ToolRegistry};

    use super::SessionToolState;

    /// Minimal named Tool used to observe semantic partition behavior.
    struct NamedTool(&'static str);

    #[async_trait]
    impl AgentTool for NamedTool {
        /// Returns one stable model-facing definition.
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.to_string(),
                description: self.0.to_string(),
                parameters: serde_json::json!({ "type": "object" }),
            }
        }

        /// Returns a deterministic empty result because partition tests do not execute Tools.
        async fn execute(
            &self,
            call: ToolCall,
            _context: &ToolExecutionContext,
        ) -> Result<ToolResult, tools::ToolError> {
            Ok(ToolResult::builder()
                .tool_call_id(call.tool_call_id)
                .blocks(Vec::new())
                .is_error(false)
                .build())
        }
    }

    impl NamedTool {
        /// Wraps this named implementation in one single-entry registry.
        fn registry(self) -> ToolRegistry {
            let mut registry = ToolRegistry::default();
            registry.register(Arc::new(self)).expect("unique Tool name");
            registry
        }
    }

    /// Verifies MCP replacement preserves local partitions and remembered user disablement.
    #[test]
    fn mcp_partition_replacement_is_atomic_and_preference_aware() {
        let state = SessionToolState::new(
            NamedTool("read").registry(),
            NamedTool("extension").registry(),
            NamedTool("mcp__old").registry(),
            vec![
                "read".to_string(),
                "extension".to_string(),
                "mcp__old".to_string(),
            ],
        )
        .expect("initial state");
        state
            .set_active(vec!["read".to_string(), "extension".to_string()])
            .expect("disable old MCP Tool");

        state
            .replace_mcp_partition(NamedTool("mcp__new").registry())
            .expect("replace MCP partition");
        let snapshot = state.snapshot().expect("snapshot");
        assert_eq!(
            snapshot.available_names(),
            vec!["extension", "mcp__new", "read"]
        );
        assert_eq!(
            snapshot.active_names(),
            vec!["extension", "mcp__new", "read"]
        );

        state
            .replace_mcp_partition(NamedTool("mcp__old").registry())
            .expect("restore old MCP Tool");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["extension", "read"]
        );
    }

    /// User changes to visible Tools retain disabled preferences for unavailable MCP Tools.
    #[test]
    fn active_changes_preserve_unavailable_tool_preferences() {
        let state = SessionToolState::new(
            NamedTool("read").registry(),
            NamedTool("extension").registry(),
            NamedTool("mcp__old").registry(),
            vec![
                "read".to_string(),
                "extension".to_string(),
                "mcp__old".to_string(),
            ],
        )
        .expect("initial state");
        state
            .set_active(vec!["read".to_string(), "extension".to_string()])
            .expect("disable MCP Tool");
        state
            .replace_mcp_partition(ToolRegistry::default())
            .expect("MCP Tool becomes unavailable");
        state
            .set_active(vec!["read".to_string()])
            .expect("change visible Tool selection");

        state
            .replace_mcp_partition(NamedTool("mcp__old").registry())
            .expect("restore MCP Tool");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["read"]
        );
    }
}
