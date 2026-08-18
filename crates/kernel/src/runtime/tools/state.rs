use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use ::tools::{AgentTool, ToolError, ToolRegistry};

/// Immutable available and active tool state captured for one Turn.
#[derive(Clone)]
pub(in crate::runtime) struct SessionToolSnapshot {
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
    pub(in crate::runtime) fn active_registry(
        &self,
    ) -> Result<ToolRegistry, ToolError> {
        self.available_registry()
            .subset(self.active.iter().map(String::as_str))
    }

    /// Returns active names in deterministic lexical order.
    pub(in crate::runtime) fn active_names(&self) -> Vec<String> {
        self.active.iter().cloned().collect()
    }

    /// Returns all available names in deterministic lexical order.
    pub(in crate::runtime) fn available_names(&self) -> Vec<String> {
        self.available_registry()
            .names()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Validates active names against this exact available registry.
    pub(in crate::runtime) fn validate_active(
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
pub(in crate::runtime) struct SessionToolState {
    current: RwLock<Arc<SessionToolSnapshot>>,
}

impl SessionToolState {
    /// Creates session Tool state while preserving temporarily unavailable preferences.
    pub(in crate::runtime) fn new(
        builtins: ToolRegistry,
        extensions: ToolRegistry,
        mcp: ToolRegistry,
        active: Vec<String>,
    ) -> Result<Self, ToolError> {
        let requested_active = active.into_iter().collect::<BTreeSet<_>>();
        let mut snapshot = SessionToolSnapshot {
            builtins,
            extensions,
            mcp,
            active: BTreeSet::new(),
            disabled_preferences: BTreeSet::new(),
            revision: 1,
        };
        let available = snapshot.available_names();
        // Restored names can refer to dynamic Tools that reconnect after the
        // Session is built, so only project currently available names as active.
        snapshot.active = available
            .iter()
            .filter(|name| requested_active.contains(*name))
            .cloned()
            .collect();
        snapshot.disabled_preferences = available
            .into_iter()
            .filter(|name| !requested_active.contains(name))
            .collect();
        Ok(Self {
            current: RwLock::new(Arc::new(snapshot)),
        })
    }

    /// Clones the current immutable snapshot and releases the state lock.
    pub(in crate::runtime) fn snapshot(
        &self,
    ) -> Result<Arc<SessionToolSnapshot>, ToolError> {
        self.current
            .read()
            .map(|snapshot| Arc::clone(&snapshot))
            .map_err(|_poison_error| ToolError::RegistryPoisoned)
    }

    /// Applies a selection against the same immutable snapshot used for validation.
    pub(in crate::runtime) fn set_active(
        &self,
        observed: &SessionToolSnapshot,
        names: Vec<String>,
    ) -> Result<(), ToolError> {
        observed.validate_active(&names)?;
        let selected = names.into_iter().collect::<BTreeSet<_>>();
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| ToolError::RegistryPoisoned)?;
        let mut next = current.as_ref().clone();
        // Only Tools visible in the validated snapshot participate in this
        // selection. Concurrent arrivals retain their existing preference.
        for name in observed.available_names() {
            if selected.contains(&name) {
                next.disabled_preferences.remove(&name);
            } else {
                next.disabled_preferences.insert(name);
            }
        }
        // Rebuild from the current catalog so concurrently removed Tools stay
        // as reconnectable preferences without leaking into active_registry().
        next.active = next
            .available_names()
            .into_iter()
            .filter(|name| !next.disabled_preferences.contains(name))
            .collect();
        next.revision = next.revision.saturating_add(1);
        *current = Arc::new(next);
        Ok(())
    }

    /// Registers and activates one tool in the same published snapshot.
    pub(in crate::runtime) fn register(
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
    pub(in crate::runtime) fn replace_mcp_partition(
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

    use ::tools::{AgentTool, ToolExecutionContext, ToolRegistry};
    use async_trait::async_trait;
    use protocol::{ToolCall, ToolDefinition, ToolResult};

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
        ) -> Result<ToolResult, ::tools::ToolError> {
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
        let observed = state.snapshot().expect("observed snapshot");
        state
            .set_active(
                &observed,
                vec!["read".to_string(), "extension".to_string()],
            )
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
        let observed = state.snapshot().expect("observed snapshot");
        state
            .set_active(
                &observed,
                vec!["read".to_string(), "extension".to_string()],
            )
            .expect("disable MCP Tool");
        state
            .replace_mcp_partition(ToolRegistry::default())
            .expect("MCP Tool becomes unavailable");
        let observed = state.snapshot().expect("observed snapshot");
        state
            .set_active(&observed, vec!["read".to_string()])
            .expect("change visible Tool selection");

        state
            .replace_mcp_partition(NamedTool("mcp__old").registry())
            .expect("restore MCP Tool");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["read"]
        );
    }

    /// Restored preferences keep a temporarily unavailable Tool selected for reconnect.
    #[test]
    fn restored_active_tool_can_be_temporarily_unavailable() {
        let state = SessionToolState::new(
            NamedTool("read").registry(),
            ToolRegistry::default(),
            ToolRegistry::default(),
            vec!["read".to_string(), "mcp__offline".to_string()],
        )
        .expect("restore state while MCP Tool is offline");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["read"]
        );

        state
            .replace_mcp_partition(NamedTool("mcp__offline").registry())
            .expect("restore MCP Tool");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["mcp__offline", "read"]
        );
    }

    /// A selection validated from one snapshot survives a concurrent catalog removal.
    #[test]
    fn active_selection_survives_catalog_change_before_publish() {
        let state = SessionToolState::new(
            NamedTool("read").registry(),
            ToolRegistry::default(),
            NamedTool("mcp__old").registry(),
            vec!["read".to_string(), "mcp__old".to_string()],
        )
        .expect("initial state");
        let observed = state.snapshot().expect("observed snapshot");
        let selected = vec!["read".to_string(), "mcp__old".to_string()];
        observed
            .validate_active(&selected)
            .expect("selection is valid in observed snapshot");
        state
            .replace_mcp_partition(ToolRegistry::default())
            .expect("MCP Tool becomes unavailable");

        state
            .set_active(&observed, selected)
            .expect("publish previously validated selection");
        state
            .replace_mcp_partition(NamedTool("mcp__old").registry())
            .expect("restore MCP Tool");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["mcp__old", "read"]
        );
    }
}
