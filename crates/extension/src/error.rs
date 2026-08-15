use protocol::ExtensionId;

/// Typed failures surfaced while registering or invoking extensions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionError {
    /// The generated binary catalogue declared one extension more than once.
    #[error("compiled extension already registered: {0}")]
    DuplicateCompiledExtension(ExtensionId),
    /// Runtime configuration selected one extension more than once.
    #[error("configured extension appears more than once: {0}")]
    DuplicateConfiguredExtension(ExtensionId),
    /// Runtime configuration requested an extension absent from this binary.
    #[error("extension is not compiled into this binary: {0}")]
    ExtensionNotCompiled(ExtensionId),
    /// A generated declaration did not match the module created by its factory.
    #[error(
        "compiled extension descriptor mismatch: expected {expected:?}, got {actual:?}"
    )]
    DescriptorMismatch {
        /// Descriptor emitted by the generated catalogue.
        expected: Box<protocol::ExtensionDescriptor>,
        /// Descriptor returned by the concrete module.
        actual: Box<protocol::ExtensionDescriptor>,
    },
    /// A selected extension omitted or changed its static descriptor declaration.
    #[error("invalid static descriptor registration for extension: {0}")]
    InvalidStaticDescriptor(ExtensionId),
    /// An extension module identifier was registered more than once.
    #[error("extension already registered: {0}")]
    DuplicateExtension(ExtensionId),
    /// A command name was registered twice by the same extension.
    #[error("extension {extension_id} already registered command {name}")]
    DuplicateCommand {
        /// Extension that owns the conflicting command.
        extension_id: ExtensionId,
        /// Conflicting short command name.
        name: String,
    },
    /// A command-line flag name was registered more than once.
    #[error("extension flag already registered: {0}")]
    DuplicateFlag(String),
    /// A provider overlay name was registered by more than one extension.
    #[error("extension provider already registered: {0}")]
    DuplicateProvider(String),
    /// A parsed extension flag value was supplied more than once.
    #[error("extension flag value already registered: {0}")]
    DuplicateFlagValue(String),
    /// A typed registration was attempted outside module registration.
    #[error("extension registration requires an active module")]
    MissingModule,
    /// An extension handler failed at runtime.
    #[error("extension handler failed: {0}")]
    Handler(String),
    /// Extension factory construction failed.
    #[error("extension factory failed: {0}")]
    Factory(String),
}
