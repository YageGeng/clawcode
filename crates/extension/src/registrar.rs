use std::collections::BTreeMap;
use std::sync::Arc;

use protocol::{
    ExtensionCommandDefinition, ExtensionDescriptor, ExtensionFlagDefinition,
    ExtensionId,
};

use crate::{
    ExtensionCommandHandler, ExtensionError, ExtensionHandler, ExtensionModule,
    HandlerRegistry, RegisterPoint, RegisteredCommand,
};

/// Mutable, build-only collector that freezes into one session registry.
#[derive(Default)]
pub struct ExtensionRegistrar {
    registry: HandlerRegistry,
    descriptors: BTreeMap<ExtensionId, ExtensionDescriptor>,
    current_extension: Option<ExtensionId>,
}

impl ExtensionRegistrar {
    /// Creates an empty typed extension registrar.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one module transactionally so failures cannot leak partial state.
    pub fn register_module(
        &mut self,
        module: &dyn ExtensionModule,
    ) -> Result<(), ExtensionError> {
        let descriptor = module.descriptor();
        if self.descriptors.contains_key(&descriptor.id) {
            return Err(ExtensionError::DuplicateExtension(descriptor.id));
        }

        // Module code writes into a complete candidate because handlers, flags,
        // commands, and tools must either become visible together or not at all.
        let mut candidate = Self {
            registry: self.registry.clone(),
            descriptors: self.descriptors.clone(),
            current_extension: Some(descriptor.id.clone()),
        };
        module.register(&mut candidate)?;
        candidate.current_extension = None;
        candidate
            .descriptors
            .insert(descriptor.id.clone(), descriptor);
        *self = candidate;
        Ok(())
    }

    /// Registers one handler with compile-time event and output checking.
    pub fn on<P, H>(&mut self, handler: H) -> Result<(), ExtensionError>
    where
        P: RegisterPoint,
        H: ExtensionHandler<P> + 'static,
    {
        let extension_id = self
            .current_extension
            .clone()
            .ok_or(ExtensionError::MissingModule)?;
        self.registry.push::<P>(extension_id, Arc::new(handler));
        Ok(())
    }

    /// Registers one command implementation under the active module identity.
    pub fn register_command<H>(
        &mut self,
        definition: ExtensionCommandDefinition,
        handler: H,
    ) -> Result<(), ExtensionError>
    where
        H: ExtensionCommandHandler + 'static,
    {
        let extension_id = self
            .current_extension
            .clone()
            .ok_or(ExtensionError::MissingModule)?;
        let qualified_name = format!("{extension_id}:{}", definition.name);
        if self.registry.commands.contains_key(&qualified_name) {
            return Err(ExtensionError::DuplicateCommand {
                extension_id,
                name: definition.name,
            });
        }
        self.registry.commands.insert(
            qualified_name,
            RegisteredCommand {
                extension_id,
                definition,
                handler: Arc::new(handler),
            },
        );
        Ok(())
    }

    /// Registers one globally unique immutable command-line flag.
    pub fn register_flag(
        &mut self,
        flag: ExtensionFlagDefinition,
    ) -> Result<(), ExtensionError> {
        if self.registry.flags.contains_key(&flag.name) {
            return Err(ExtensionError::DuplicateFlag(flag.name));
        }
        self.registry.flags.insert(flag.name.clone(), flag);
        Ok(())
    }

    /// Registers one tool contributed by the active module.
    pub fn register_tool(
        &mut self,
        tool: Arc<dyn tools::AgentTool>,
    ) -> Result<(), ExtensionError> {
        let extension_id = self
            .current_extension
            .clone()
            .ok_or(ExtensionError::MissingModule)?;
        let name = tool.definition().name;
        // Pi resolves colliding extension tools by stable registration order.
        if self
            .registry
            .tools
            .iter()
            .any(|(_owner, registered)| registered.definition().name == name)
        {
            return Ok(());
        }
        self.registry.tools.push((extension_id, tool));
        Ok(())
    }

    /// Freezes this build-only registrar into an immutable handler registry.
    #[must_use]
    pub fn freeze(mut self) -> HandlerRegistry {
        self.registry.descriptors = self.descriptors;
        self.registry
    }
}
