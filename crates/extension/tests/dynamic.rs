use std::sync::Arc;

use async_trait::async_trait;
use extension::{
    DynamicCommandRegistry, DynamicRegistryError, ExtensionCommandContext,
    ExtensionCommandHandler, ExtensionCommandInvocation, ExtensionError,
    RegisteredCommand,
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
            argument_hint: None,
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

    commands
        .remove(
            &ExtensionId::try_from("first").expect("extension id"),
            "inspect",
        )
        .expect("remove first command");
    let removed = commands.snapshot().expect("removed snapshot");
    assert!(removed.resolve("first/inspect").is_err());
    assert_eq!(
        removed
            .resolve("inspect")
            .expect("remaining short command")
            .extension_id
            .as_str(),
        "second"
    );
    current
        .resolve("first/inspect")
        .expect("pre-remove snapshot remains stable");

    assert_eq!(
        old.resolve("inspect")
            .expect("old snapshot remains unique")
            .extension_id
            .as_str(),
        "first"
    );
}

/// Slash parsing uses only one ordinary space as pi's command delimiter.
#[test]
fn extension_command_invocation_preserves_non_space_name_characters() {
    let invocation =
        ExtensionCommandInvocation::try_from("/first/inspect   alpha beta  ")
            .expect("command invocation");
    assert_eq!(invocation.name, "first/inspect");
    assert_eq!(invocation.arguments, "alpha beta");

    let tabbed = ExtensionCommandInvocation::try_from("/inspect\talpha")
        .expect("tabbed invocation");
    assert_eq!(tabbed.name, "inspect\talpha");
    assert!(tabbed.arguments.is_empty());

    for invalid in ["inspect", "/"] {
        assert!(matches!(
            ExtensionCommandInvocation::try_from(invalid),
            Err(DynamicRegistryError::InvalidCommandInvocation(value))
                if value == invalid
        ));
    }
}
