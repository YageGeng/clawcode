use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use ::tools::{AgentTool, ToolError, ToolRegistry};
use protocol::{ToolExposure, ToolSelection};

use crate::KernelError;

/// Immutable catalog partitions and explicit preferences captured for one model request.
#[derive(Clone, typed_builder::TypedBuilder)]
#[builder(builder_method(vis = ""), builder_type(vis = ""))]
pub(in crate::runtime) struct SessionToolSnapshot {
    builtins: ToolRegistry,
    extensions: ToolRegistry,
    mcp: ToolRegistry,
    pub(in crate::runtime) selection: ToolSelection,
    revision: u64,
    #[builder(default)]
    pub(super) search_index: Arc<super::search::ToolSearchIndex>,
}

impl SessionToolSnapshot {
    /// Applies the single visibility formula shared by model declarations and UI projections.
    pub(in crate::runtime) fn declares(
        &self,
        name: &str,
        exposure: ToolExposure,
    ) -> bool {
        exposure != ToolExposure::Hidden
            && !self.selection.disabled.contains(name)
            && (exposure == ToolExposure::Direct
                || self.selection.loaded.contains(name))
    }
    /// Rebuilds only catalog-derived documents; selection updates retain this shared index.
    fn rebuild_search_index(&mut self) {
        let available = self.available_registry();
        self.search_index = Arc::new(
            available
                .names()
                .into_iter()
                .filter_map(|name| {
                    available
                        .tool(name)
                        .filter(|tool| {
                            tool.metadata().exposure == ToolExposure::Deferred
                        })
                        .map(|tool| {
                            super::search::ToolSearchDocument::from((
                                name,
                                tool.as_ref(),
                            ))
                        })
                })
                .collect(),
        );
    }
    /// Composes semantic partitions in their stable override order.
    pub(super) fn available_registry(&self) -> ToolRegistry {
        let mut available = self.builtins.clone();
        available.overlay(&self.extensions);
        available.overlay(&self.mcp);
        available
    }

    /// Derives declarations from policy and selection instead of storing another active truth.
    pub(in crate::runtime) fn active_registry(
        &self,
    ) -> Result<ToolRegistry, ToolError> {
        let available = self.available_registry();
        available.subset(self.active_names().iter().map(String::as_str))
    }

    /// Returns deterministic visible names under the frozen host policy.
    pub(in crate::runtime) fn active_names(&self) -> Vec<String> {
        let available = self.available_registry();
        available
            .names()
            .into_iter()
            .filter(|name| {
                if *name == "tool_search"
                    && !self.search_index.available(&self.selection)
                {
                    return false;
                }
                available.tool(name).is_some_and(|tool| {
                    let exposure = tool.metadata().exposure;
                    self.declares(name, exposure)
                })
            })
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Returns selectable names; Hidden implementations stay out of discovery APIs.
    pub(in crate::runtime) fn available_names(&self) -> Vec<String> {
        let available = self.available_registry();
        available
            .names()
            .into_iter()
            .filter(|name| {
                available.tool(name).is_some_and(|tool| {
                    tool.metadata().exposure != ToolExposure::Hidden
                })
            })
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Validates complete selections without letting Hidden tools bypass their policy.
    pub(in crate::runtime) fn validate_active(
        &self,
        names: &[String],
    ) -> Result<(), ToolError> {
        let available = self.available_registry();
        for name in names {
            if available.tool(name).is_none_or(|tool| {
                tool.metadata().exposure == ToolExposure::Hidden
            }) {
                return Err(ToolError::NotFound(name.clone()));
            }
        }
        Ok(())
    }

    /// Replaces only observed candidates so concurrent arrivals preserve their own preferences.
    pub(super) fn replace_selection(
        &mut self,
        observed: &Self,
        names: Vec<String>,
    ) -> Result<(), ToolError> {
        observed.validate_active(&names)?;
        let selected: BTreeSet<_> = names.into_iter().collect();
        let current = self.available_registry();
        for name in &selected {
            if current.tool(name).is_some_and(|tool| {
                tool.metadata().exposure == ToolExposure::Hidden
            }) {
                return Err(ToolError::NotFound(name.clone()));
            }
        }
        for name in observed.available_names() {
            if selected.contains(&name) {
                self.selection.disabled.remove(&name);
                self.selection.loaded.insert(name);
            } else {
                self.selection.loaded.remove(&name);
                self.selection.disabled.insert(name);
            }
        }
        Ok(())
    }

    /// Migrates only uniquely mapped old names while retaining preferences for absent tools.
    pub(super) fn migrate_selection(&self, selection: &mut ToolSelection) {
        let catalog = self.available_registry();
        let mut aliases =
            std::collections::BTreeMap::<String, Option<String>>::new();
        for name in catalog.names() {
            if let Some(tool) = catalog.tool(name)
                && let Some(reference) = &tool.metadata().mcp_ref
            {
                let old = format!(
                    "mcp__{}__{}",
                    reference.server_id, reference.remote_name
                );
                aliases
                    .entry(old)
                    .and_modify(|value| *value = None)
                    .or_insert_with(|| Some(name.to_string()));
            }
        }
        for names in [&mut selection.loaded, &mut selection.disabled] {
            *names = std::mem::take(names)
                .into_iter()
                .map(|name| {
                    if catalog.tool(&name).is_none() {
                        aliases
                            .get(&name)
                            .and_then(Clone::clone)
                            .unwrap_or(name)
                    } else {
                        name
                    }
                })
                .collect();
        }
        // Explicit disablement wins if old and new aliases both survived in a historical entry.
        selection
            .loaded
            .retain(|name| !selection.disabled.contains(name));
    }
}

/// The write lock serializes selection transactions and catalog publication without a Run gate.
pub(in crate::runtime) struct SessionToolState {
    current: RwLock<Arc<SessionToolSnapshot>>,
}

impl SessionToolState {
    /// Creates independent catalog state with retained explicit preferences.
    pub(in crate::runtime) fn new(
        builtins: ToolRegistry,
        extensions: ToolRegistry,
        mcp: ToolRegistry,
        selection: impl Into<ToolSelection>,
    ) -> Result<Self, ToolError> {
        let mut snapshot = SessionToolSnapshot::builder()
            .builtins(builtins)
            .extensions(extensions)
            .mcp(mcp)
            .selection(selection.into())
            .revision(1)
            .build();
        snapshot.rebuild_search_index();
        Ok(Self {
            current: RwLock::new(Arc::new(snapshot)),
        })
    }

    /// Clones the current immutable snapshot and immediately releases the read lock.
    pub(in crate::runtime) fn snapshot(
        &self,
    ) -> Result<Arc<SessionToolSnapshot>, ToolError> {
        self.current
            .read()
            .map(|snapshot| Arc::clone(&snapshot))
            .map_err(|_poison_error| ToolError::RegistryPoisoned)
    }

    /// Publishes a prepared selection only after its caller's synchronous Store commit succeeds.
    pub(in crate::runtime) fn update<R>(
        &self,
        update: impl FnOnce(&mut SessionToolSnapshot) -> Result<R, KernelError>,
    ) -> Result<R, KernelError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| ToolError::RegistryPoisoned)?;
        let mut next = current.as_ref().clone();
        let result = update(&mut next)?;
        next.revision = next.revision.saturating_add(1);
        *current = Arc::new(next);
        Ok(result)
    }

    /// Registers a catalog entry without implicitly loading Deferred or Hidden tools.
    pub(in crate::runtime) fn register(
        &self,
        tool: Arc<dyn AgentTool>,
    ) -> Result<(), ToolError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| ToolError::RegistryPoisoned)?;
        let mut next = current.as_ref().clone();
        next.extensions.upsert(tool);
        next.rebuild_search_index();
        next.revision = next.revision.saturating_add(1);
        *current = Arc::new(next);
        Ok(())
    }

    /// Replaces only the catalog and migrates newly resolvable historical preferences.
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
        next.rebuild_search_index();
        let mut selection = next.selection.clone();
        next.migrate_selection(&mut selection);
        next.selection = selection;
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
        /// Uses name categories only inside this fixture to exercise host visibility.
        fn metadata(&self) -> &protocol::ToolMetadata {
            static DEFERRED: std::sync::LazyLock<protocol::ToolMetadata> =
                std::sync::LazyLock::new(|| {
                    protocol::ToolMetadata::builder()
                        .exposure(protocol::ToolExposure::Deferred)
                        .build()
                });
            static HIDDEN: std::sync::LazyLock<protocol::ToolMetadata> =
                std::sync::LazyLock::new(|| {
                    protocol::ToolMetadata::builder()
                        .exposure(protocol::ToolExposure::Hidden)
                        .build()
                });
            static DIRECT: std::sync::LazyLock<protocol::ToolMetadata> =
                std::sync::LazyLock::new(|| {
                    protocol::ToolMetadata::builder().build()
                });
            match self.0 {
                "deferred" => &DEFERRED,
                "hidden" => &HIDDEN,
                _ => &DIRECT,
            }
        }
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

    /// Catalog arrivals cannot activate Deferred tools and Hidden tools cannot be selected.
    #[test]
    fn catalog_refresh_respects_exposure() {
        let state = SessionToolState::new(
            NamedTool("read").registry(),
            ToolRegistry::default(),
            ToolRegistry::default(),
            vec!["read".into()],
        )
        .expect("state");
        state
            .replace_mcp_partition(NamedTool("deferred").registry())
            .expect("catalog");
        assert_eq!(
            state.snapshot().expect("snapshot").active_names(),
            vec!["read"]
        );
        state
            .replace_mcp_partition(NamedTool("hidden").registry())
            .expect("catalog");
        state
            .snapshot()
            .expect("snapshot")
            .validate_active(&["hidden".into()])
            .expect_err("hidden is not selectable");
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
            .update(|next| {
                next.replace_selection(
                    &observed,
                    vec!["read".to_string(), "extension".to_string()],
                )
                .map_err(Into::into)
            })
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
            .update(|next| {
                next.replace_selection(
                    &observed,
                    vec!["read".to_string(), "extension".to_string()],
                )
                .map_err(Into::into)
            })
            .expect("disable MCP Tool");
        state
            .replace_mcp_partition(ToolRegistry::default())
            .expect("MCP Tool becomes unavailable");
        let observed = state.snapshot().expect("observed snapshot");
        state
            .update(|next| {
                next.replace_selection(&observed, vec!["read".to_string()])
                    .map_err(Into::into)
            })
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
            .update(|next| {
                next.replace_selection(&observed, selected)
                    .map_err(Into::into)
            })
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
