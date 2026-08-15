//! Typed, session-isolated, non-UI Pi-compatible extension lifecycle.

mod catalog;
mod context;
mod dynamic;
mod error;
mod factory;
mod handler;
mod host;
mod point;
mod registrar;
mod registry;
mod runtime;

pub use catalog::{CompiledExtension, ExtensionCatalog};
pub use context::{
    ExtensionCommandContext, ExtensionContext, RuntimeGeneration,
};
pub use dynamic::{
    CommandRegistry, DynamicCommandRegistry, DynamicRegistryError,
};
pub use error::ExtensionError;
pub use factory::{
    ExtensionFactory, ExtensionModule, ExtensionModuleFactory,
    StaticExtensionFactory,
};
pub use handler::{ExtensionCommandHandler, ExtensionHandler};
pub use host::{ExtensionHost, ExtensionHostError, UnsupportedExtensionHost};
pub use point::*;
pub use registrar::ExtensionRegistrar;
pub use registry::{
    HandlerRegistry, RegisterPoint, RegisteredCommand, RegisteredHandler,
};
pub use runtime::{
    DiscardExtensionDiagnostics, ExtensionDiagnosticSink, ExtensionRuntime,
};
