//! Provider sign-in values shared by the provider, kernel, and protocol adapters.

use std::fmt;
use std::sync::Arc;

/// One way a client can sign in to a provider; holds no credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginMethod {
    /// Stable method identifier, which is the provider identifier.
    pub id: String,
    /// Human-readable provider name.
    pub name: String,
}

/// What a user must do to authorize a device sign-in.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// Page where the user enters the code.
    pub verification_uri: String,
    /// Short-lived code that authorizes this sign-in.
    pub user_code: String,
}

impl fmt::Debug for DeviceCode {
    /// Keeps the code out of any log that formats this value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCode")
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &"<redacted>")
            .finish()
    }
}

/// Receives the device code as soon as a sign-in obtains one.
pub type DeviceCodeSink = Arc<dyn Fn(DeviceCode) + Send + Sync>;
