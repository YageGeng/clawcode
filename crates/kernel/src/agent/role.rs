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
                .description("Lightweight agent for fast codebase exploration".to_string())
                .config_overrides({
                    let mut m = HashMap::new();
                    m.insert("reasoning_effort".to_string(), "low".to_string());
                    m
                })
                .prompt(include_str!("../prompts/explorer.txt").to_string())
                .build(),
        );
        set.insert(
            AgentRole::builder()
                .name("worker".to_string())
                .description("Full-capability agent for implementation work".to_string())
                .config_overrides({
                    let mut m = HashMap::new();
                    m.insert("reasoning_effort".to_string(), "high".to_string());
                    m
                })
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
    fn explorer_overrides_reasoning() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("explorer").unwrap();
        assert_eq!(role.reasoning_override(), Some("low"));
    }

    #[test]
    fn worker_overrides_reasoning() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("worker").unwrap();
        assert_eq!(role.reasoning_override(), Some("high"));
    }

    #[test]
    fn default_has_no_overrides() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("default").unwrap();
        assert!(role.model_override().is_none());
        assert!(role.reasoning_override().is_none());
    }
}
