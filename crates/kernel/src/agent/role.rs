//! Agent role system: config overlays applied at spawn time.
//!
//! Roles define model, reasoning, and system-prompt overrides that
//! are layered onto the parent agent's configuration when spawning
//! a sub-agent. Built-in roles are "default", "explorer", and "worker".

use std::collections::HashMap;

/// A named role with optional configuration overrides.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct AgentRole {
    pub name: String,
    pub description: String,
    #[builder(default)]
    pub nickname_candidates: Vec<String>,
    /// Config overrides as key-value pairs.
    /// Supported keys: "model", "reasoning_effort".
    #[builder(default)]
    pub config_overrides: HashMap<String, String>,
    /// Agent-specific system prompt. When `Some`, completely replaces the
    /// default system prompt. When `None`, the default is used.
    #[builder(default, setter(strip_option))]
    pub prompt: Option<String>,
}

/// A set of agent roles, keyed by role name.
#[derive(Clone, Debug, Default)]
pub struct AgentRoleSet {
    roles: HashMap<String, AgentRole>,
}

impl AgentRoleSet {
    /// Create a role set with the three built-in roles.
    pub(crate) fn with_builtins() -> Self {
        let mut set = Self::default();
        set.insert(
            AgentRole::builder()
                .name("default".to_string())
                .description("No overrides, full parent config inheritance".to_string())
                .build(),
        );
        set.insert(
            AgentRole::builder()
                .name("explorer".to_string())
                .description("Agent for specific, well-scoped codebase exploration".to_string())
                .prompt(include_str!("../prompts/explorer.txt").to_string())
                .build(),
        );
        set.insert(
            AgentRole::builder()
                .name("worker".to_string())
                .description("Agent for implementation, bug fixing, and tests".to_string())
                .prompt(include_str!("../prompts/worker.txt").to_string())
                .build(),
        );
        set
    }

    /// Look up a role by name. Returns `None` if not found.
    pub(crate) fn get(&self, name: &str) -> Option<&AgentRole> {
        self.roles.get(name)
    }

    /// Insert a role, replacing any existing role with the same name.
    pub(crate) fn insert(&mut self, role: AgentRole) {
        self.roles.insert(role.name.clone(), role);
    }
}

impl AgentRole {
    /// Resolve the effective model_id for this role.
    /// Returns `None` if the role does not override the model.
    pub(crate) fn model_override(&self) -> Option<&str> {
        self.config_overrides.get("model").map(|s| s.as_str())
    }

    /// Resolve the reasoning effort for this role.
    /// Returns `None` if the role does not override reasoning.
    #[allow(dead_code)]
    pub(crate) fn reasoning_override(&self) -> Option<&str> {
        self.config_overrides
            .get("reasoning_effort")
            .map(|s| s.as_str())
    }

    /// Resolve the system prompt override for this role.
    pub(crate) fn prompt_override(&self) -> Option<&str> {
        self.prompt.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_present() {
        let set = AgentRoleSet::with_builtins();
        assert!(set.get("default").is_some());
        assert!(set.get("explorer").is_some());
        assert!(set.get("worker").is_some());
    }

    #[test]
    fn explorer_inherits_reasoning() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("explorer").unwrap();
        assert!(role.reasoning_override().is_none());
    }

    #[test]
    fn worker_inherits_reasoning() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("worker").unwrap();
        assert!(role.reasoning_override().is_none());
    }

    #[test]
    fn default_has_no_overrides() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("default").unwrap();
        assert!(role.model_override().is_none());
        assert!(role.reasoning_override().is_none());
    }

    #[test]
    fn explorer_and_worker_have_prompt_overrides() {
        let set = AgentRoleSet::with_builtins();

        assert!(set.get("explorer").unwrap().prompt_override().is_some());
        assert!(set.get("worker").unwrap().prompt_override().is_some());
    }
}
