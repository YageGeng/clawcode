use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use protocol::ExtensionId;

use crate::RegisteredCommand;

/// Failures produced by versioned command and active-tool registries.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DynamicRegistryError {
    /// One qualified command was supplied more than once during construction.
    #[error("extension command already registered: {0}")]
    DuplicateCommand(String),
    /// No command matched the requested qualified or short name.
    #[error("extension command not found: {0}")]
    CommandNotFound(String),
    /// More than one extension owns the requested short command name.
    #[error(
        "extension command name is ambiguous: {name}; use one of: {candidates:?}"
    )]
    AmbiguousCommand {
        /// Ambiguous short command name.
        name: String,
        /// Stable qualified command alternatives.
        candidates: Vec<String>,
    },
    /// A dynamic registry lock was poisoned by a panicking writer.
    #[error("extension dynamic registry lock poisoned")]
    Poisoned,
}

impl RegisteredCommand {
    /// Returns the stable extension-qualified command name.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        format!("{}:{}", self.extension_id, self.definition.name)
    }
}

/// Immutable command snapshot used by one in-flight dispatch.
#[derive(Default, Clone)]
pub struct CommandRegistry {
    commands: BTreeMap<String, RegisteredCommand>,
}

impl CommandRegistry {
    /// Builds a command snapshot while rejecting duplicate qualified names.
    pub fn new(
        commands: Vec<RegisteredCommand>,
    ) -> Result<Self, DynamicRegistryError> {
        let mut registry = Self::default();
        for command in commands {
            let qualified_name = command.qualified_name();
            if registry.commands.contains_key(&qualified_name) {
                return Err(DynamicRegistryError::DuplicateCommand(
                    qualified_name,
                ));
            }
            registry.commands.insert(qualified_name, command);
        }
        Ok(registry)
    }

    /// Resolves a qualified name directly or a globally unique short name.
    pub fn resolve(
        &self,
        name: &str,
    ) -> Result<RegisteredCommand, DynamicRegistryError> {
        if let Some(command) = self.commands.get(name) {
            return Ok(command.clone());
        }
        let matches = self
            .commands
            .values()
            .filter(|command| command.definition.name == name)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [command] => Ok((*command).clone()),
            [] => Err(DynamicRegistryError::CommandNotFound(name.to_string())),
            many => Err(DynamicRegistryError::AmbiguousCommand {
                name: name.to_string(),
                candidates: many
                    .iter()
                    .map(|command| command.qualified_name())
                    .collect(),
            }),
        }
    }

    /// Returns command registrations in qualified lexical order.
    #[must_use]
    pub fn commands(&self) -> Vec<RegisteredCommand> {
        self.commands.values().cloned().collect()
    }

    /// Inserts or replaces one qualified command in this mutable candidate.
    fn upsert(&mut self, command: RegisteredCommand) {
        self.commands.insert(command.qualified_name(), command);
    }

    /// Removes one command only through its owning Extension-qualified key.
    fn remove(&mut self, extension_id: &ExtensionId, name: &str) -> bool {
        self.commands
            .remove(&format!("{extension_id}:{name}"))
            .is_some()
    }
}

/// Session-local command registry that publishes immutable snapshots.
pub struct DynamicCommandRegistry {
    current: RwLock<Arc<CommandRegistry>>,
}

impl DynamicCommandRegistry {
    /// Creates a dynamic command registry from initial registrations.
    pub fn new(
        commands: Vec<RegisteredCommand>,
    ) -> Result<Self, DynamicRegistryError> {
        Ok(Self {
            current: RwLock::new(Arc::new(CommandRegistry::new(commands)?)),
        })
    }

    /// Clones the current immutable snapshot and immediately releases its lock.
    pub fn snapshot(
        &self,
    ) -> Result<Arc<CommandRegistry>, DynamicRegistryError> {
        let current = self
            .current
            .read()
            .map_err(|_poison_error| DynamicRegistryError::Poisoned)?;
        Ok(Arc::clone(&current))
    }

    /// Publishes a new snapshot containing an inserted or replaced command.
    pub fn upsert(
        &self,
        command: RegisteredCommand,
    ) -> Result<(), DynamicRegistryError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| DynamicRegistryError::Poisoned)?;
        let mut next = current.as_ref().clone();
        next.upsert(command);
        *current = Arc::new(next);
        Ok(())
    }

    /// Publishes a new snapshot without one command owned by the caller.
    pub fn remove(
        &self,
        extension_id: &ExtensionId,
        name: &str,
    ) -> Result<bool, DynamicRegistryError> {
        let mut current = self
            .current
            .write()
            .map_err(|_poison_error| DynamicRegistryError::Poisoned)?;
        let mut next = current.as_ref().clone();
        let removed = next.remove(extension_id, name);
        if removed {
            *current = Arc::new(next);
        }
        Ok(removed)
    }
}
