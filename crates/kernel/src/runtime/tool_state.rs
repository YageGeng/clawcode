use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use tools::{AgentTool, ToolError, ToolRegistry};

/// Immutable available and active tool state captured for one Turn.
#[derive(Clone)]
pub(super) struct SessionToolSnapshot {
    available: ToolRegistry,
    active: BTreeSet<String>,
}

impl SessionToolSnapshot {
    /// Builds the model-facing registry selected by this immutable snapshot.
    pub(super) fn active_registry(&self) -> Result<ToolRegistry, ToolError> {
        self.available
            .subset(self.active.iter().map(String::as_str))
    }

    /// Returns active names in deterministic lexical order.
    pub(super) fn active_names(&self) -> Vec<String> {
        self.active.iter().cloned().collect()
    }

    /// Returns all available names in deterministic lexical order.
    pub(super) fn available_names(&self) -> Vec<String> {
        self.available
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
        for name in names {
            if self.available.tool(name).is_none() {
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
        available: ToolRegistry,
        active: Vec<String>,
    ) -> Result<Self, ToolError> {
        let snapshot = SessionToolSnapshot {
            available,
            active: active.into_iter().collect(),
        };
        snapshot.validate_active(&snapshot.active_names())?;
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
        next.available.upsert(tool);
        next.active.insert(name);
        *current = Arc::new(next);
        Ok(())
    }
}
