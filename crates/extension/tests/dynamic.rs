use std::sync::Arc;

use async_trait::async_trait;
use extension::{
    DynamicCommandRegistry, ExtensionCommandContext, ExtensionCommandHandler,
    ExtensionError, RegisteredCommand,
};
use protocol::{ExtensionCommandDefinition, ExtensionId};

/// Command implementation used only to establish distinct registrations.
struct CommandHandler;

#[async_trait]
impl ExtensionCommandHandler for CommandHandler {
    /// Accepts a resolved command invocation without host actions.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        _context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
}

/// Creates one qualified command registration.
fn command(extension_id: &str, name: &str) -> RegisteredCommand {
    RegisteredCommand {
        extension_id: ExtensionId::try_from(extension_id)
            .expect("extension id"),
        definition: ExtensionCommandDefinition {
            name: name.to_string(),
            description: None,
        },
        handler: Arc::new(CommandHandler),
    }
}

/// Dynamic command snapshots keep old dispatches stable and detect short-name ambiguity.
#[test]
fn dynamic_commands_are_versioned_and_qualified() {
    let commands =
        DynamicCommandRegistry::new(vec![command("first", "inspect")])
            .expect("initial commands");
    let old = commands.snapshot().expect("old snapshot");
    commands
        .upsert(command("second", "inspect"))
        .expect("second command");
    let current = commands.snapshot().expect("current snapshot");

    assert_eq!(
        old.resolve("inspect")
            .expect("unique old command")
            .extension_id
            .as_str(),
        "first"
    );
    assert!(current.resolve("inspect").is_err());
    current
        .resolve("first/inspect")
        .expect("first qualified command");
    current
        .resolve("second/inspect")
        .expect("second qualified command");

    assert_eq!(
        old.resolve("inspect")
            .expect("old snapshot remains unique")
            .extension_id
            .as_str(),
        "first"
    );
}
