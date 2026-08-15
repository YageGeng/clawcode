//! Static extension selection parsed from TOML.

use protocol::ExtensionId;
use serde::{Deserialize, Serialize};

/// Stable identifier enabled when the configuration omits extension settings.
pub const DEFAULT_EXTENSION_ID: &str = "command-guard";

/// Ordered extensions selected for build-time inclusion and runtime activation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExtensionsConfig {
    /// Extension identifiers in hook execution order.
    #[serde(default = "default_extensions")]
    pub enabled: Vec<ExtensionId>,
}

impl Default for ExtensionsConfig {
    /// Enables only the built-in command guard by default.
    fn default() -> Self {
        Self {
            enabled: default_extensions(),
        }
    }
}

/// Builds the shared default used by Serde and the Rust `Default` contract.
fn default_extensions() -> Vec<ExtensionId> {
    vec![
        ExtensionId::try_from(DEFAULT_EXTENSION_ID)
            .expect("the built-in extension identifier is valid"),
    ]
}
