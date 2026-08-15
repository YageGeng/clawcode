use serde::{Deserialize, Serialize};

use crate::ExtensionId;

/// Stable metadata for one extension module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionDescriptor {
    /// Globally unique extension identifier.
    pub id: ExtensionId,
    /// Human-readable extension name.
    pub name: String,
    /// Extension implementation version.
    pub version: String,
}

/// Metadata for one non-UI command contributed by an extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionCommandDefinition {
    /// Short command name within the owning extension.
    pub name: String,
    /// Human-readable command description.
    pub description: Option<String>,
}

/// Supported non-UI CLI flag value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionFlagKind {
    /// Boolean command-line flag.
    Boolean,
    /// String-valued command-line flag.
    String,
}

/// One CLI flag contributed during extension registration.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct ExtensionFlagDefinition {
    /// Unique flag name.
    pub name: String,
    /// Human-readable flag description.
    #[builder(default)]
    pub description: Option<String>,
    /// Accepted flag value kind.
    pub kind: ExtensionFlagKind,
    /// Optional JSON default matching the declared kind.
    #[builder(default)]
    pub default: Option<serde_json::Value>,
}

/// Provider configuration contributed before model-catalog construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionProviderRegistration {
    /// Provider identifier registered in the immutable configuration overlay.
    pub name: String,
    /// Provider-specific configuration validated by the provider factory.
    pub config: serde_json::Value,
}

/// Extension declarations resolved before any session runtime is created.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct StaticExtensionRegistration {
    /// Ordered extension descriptors.
    #[builder(default)]
    pub extensions: Vec<ExtensionDescriptor>,
    /// Globally unique command-line flags.
    #[builder(default)]
    pub flags: Vec<ExtensionFlagDefinition>,
    /// Build-time provider configuration overlays.
    #[builder(default)]
    pub providers: Vec<ExtensionProviderRegistration>,
    /// Parsed immutable values for registered flags.
    #[builder(default)]
    pub flag_values: serde_json::Map<String, serde_json::Value>,
}
