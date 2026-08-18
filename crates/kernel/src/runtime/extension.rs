mod context;
mod diagnostics;
mod host;
mod provider;

use std::sync::Arc;

use ::extension::{
    CommandRegistry, DynamicCommandRegistry, DynamicRegistryError,
    ExtensionRuntime, RegisteredCommand,
};
use protocol::{ExtensionEventData, ExtensionFlagDefinition, ExtensionId};
use tokio::sync::broadcast;

pub(super) use diagnostics::KernelExtensionDiagnostics;

/// Groups all extension-owned state behind one Session capability boundary.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct SessionExtensions {
    runtime: Arc<ExtensionRuntime>,
    commands: DynamicCommandRegistry,
    flags: Arc<[ExtensionFlagDefinition]>,
    events: broadcast::Sender<ExtensionEventData>,
}

impl SessionExtensions {
    /// Borrows the underlying runtime for synchronous or awaited hook dispatch.
    pub(super) fn runtime_ref(&self) -> &ExtensionRuntime {
        &self.runtime
    }

    /// Returns an explicit Arc clone for request hooks that outlive a borrow.
    pub(super) fn runtime(&self) -> Arc<ExtensionRuntime> {
        Arc::clone(&self.runtime)
    }

    /// Returns the current immutable dynamic command registry snapshot.
    pub(super) fn command_snapshot(
        &self,
    ) -> Result<Arc<CommandRegistry>, DynamicRegistryError> {
        self.commands.snapshot()
    }

    /// Inserts or replaces one session-local Extension command.
    pub(super) fn upsert_command(
        &self,
        command: RegisteredCommand,
    ) -> Result<(), DynamicRegistryError> {
        self.commands.upsert(command)
    }

    /// Removes one session-local command through its owning Extension identity.
    pub(super) fn remove_command(
        &self,
        extension_id: &ExtensionId,
        name: &str,
    ) -> Result<bool, DynamicRegistryError> {
        self.commands.remove(extension_id, name)
    }

    /// Returns immutable session-local Extension flag definitions.
    pub(super) fn flags(&self) -> &[ExtensionFlagDefinition] {
        &self.flags
    }

    /// Publishes one session-local Extension event to current subscribers.
    pub(super) fn publish_event(&self, event: ExtensionEventData) {
        let _receivers = self.events.send(event);
    }

    /// Subscribes to future session-local Extension events.
    pub(super) fn subscribe_events(
        &self,
    ) -> broadcast::Receiver<ExtensionEventData> {
        self.events.subscribe()
    }
}
