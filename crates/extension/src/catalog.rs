use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use protocol::{ExtensionDescriptor, ExtensionId, StaticExtensionRegistration};

use crate::{ExtensionError, ExtensionModuleFactory, StaticExtensionFactory};

/// One concrete extension implementation compiled into the current binary.
pub struct CompiledExtension {
    descriptor: ExtensionDescriptor,
    registration: StaticExtensionRegistration,
    module_factory: ExtensionModuleFactory,
}

impl CompiledExtension {
    /// Creates one immutable entry emitted by the extension build catalogue.
    #[must_use]
    pub fn new(
        descriptor: ExtensionDescriptor,
        registration: StaticExtensionRegistration,
        module_factory: ExtensionModuleFactory,
    ) -> Self {
        Self {
            descriptor,
            registration,
            module_factory,
        }
    }

    /// Wraps the constructor so descriptor validation occurs on the real session instance.
    fn validated_module_factory(&self) -> ExtensionModuleFactory {
        let expected = self.descriptor.clone();
        let factory = Arc::clone(&self.module_factory);
        Arc::new(move || {
            let module = factory()?;
            let actual = module.descriptor();
            if actual != expected {
                return Err(ExtensionError::DescriptorMismatch {
                    expected: Box::new(expected.clone()),
                    actual: Box::new(actual),
                });
            }
            Ok(module)
        })
    }
}

/// All extension implementations that the current binary can instantiate.
pub struct ExtensionCatalog {
    entries: BTreeMap<ExtensionId, CompiledExtension>,
}

impl ExtensionCatalog {
    /// Indexes compiled entries while rejecting ambiguous generated identities.
    pub fn new(
        entries: Vec<CompiledExtension>,
    ) -> Result<Self, ExtensionError> {
        let mut indexed = BTreeMap::new();
        for entry in entries {
            let extension_id = entry.descriptor.id.clone();
            if indexed.insert(extension_id.clone(), entry).is_some() {
                return Err(ExtensionError::DuplicateCompiledExtension(
                    extension_id,
                ));
            }
        }
        Ok(Self { entries: indexed })
    }

    /// Selects runtime entries in configuration order and combines their declarations.
    pub fn select(
        &self,
        enabled: &[ExtensionId],
    ) -> Result<StaticExtensionFactory, ExtensionError> {
        let mut selected_ids = BTreeSet::new();
        let mut selection = CatalogSelection::default();
        for extension_id in enabled {
            if !selected_ids.insert(extension_id.clone()) {
                return Err(ExtensionError::DuplicateConfiguredExtension(
                    extension_id.clone(),
                ));
            }
            let entry = self.entries.get(extension_id).ok_or_else(|| {
                ExtensionError::ExtensionNotCompiled(extension_id.clone())
            })?;
            selection.append(entry)?;
        }
        Ok(selection.into_factory())
    }
}

/// Incremental aggregate that keeps registration and module order aligned.
#[derive(Default)]
struct CatalogSelection {
    registration: StaticExtensionRegistration,
    modules: Vec<ExtensionModuleFactory>,
}

impl CatalogSelection {
    /// Validates one compiled entry and appends all of its declarations atomically.
    fn append(
        &mut self,
        entry: &CompiledExtension,
    ) -> Result<(), ExtensionError> {
        if entry.registration.extensions.as_slice()
            != std::slice::from_ref(&entry.descriptor)
        {
            return Err(ExtensionError::InvalidStaticDescriptor(
                entry.descriptor.id.clone(),
            ));
        }

        {
            let mut names: BTreeSet<_> = self
                .registration
                .flags
                .iter()
                .map(|flag| flag.name.as_str())
                .collect();
            for flag in &entry.registration.flags {
                if !names.insert(flag.name.as_str()) {
                    return Err(ExtensionError::DuplicateFlag(
                        flag.name.clone(),
                    ));
                }
            }
        }
        {
            let mut names: BTreeSet<_> = self
                .registration
                .providers
                .iter()
                .map(|provider| provider.name.as_str())
                .collect();
            for provider in &entry.registration.providers {
                if !names.insert(provider.name.as_str()) {
                    return Err(ExtensionError::DuplicateProvider(
                        provider.name.clone(),
                    ));
                }
            }
        }
        for name in entry.registration.flag_values.keys() {
            if self.registration.flag_values.contains_key(name) {
                return Err(ExtensionError::DuplicateFlagValue(name.clone()));
            }
        }

        self.registration.extensions.push(entry.descriptor.clone());
        self.registration
            .flags
            .extend(entry.registration.flags.iter().cloned());
        self.registration
            .providers
            .extend(entry.registration.providers.iter().cloned());
        self.registration.flag_values.extend(
            entry
                .registration
                .flag_values
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        self.modules.push(entry.validated_module_factory());
        Ok(())
    }

    /// Converts a validated aggregate into the factory consumed by the kernel.
    fn into_factory(self) -> StaticExtensionFactory {
        StaticExtensionFactory::new(self.registration, self.modules)
    }
}
