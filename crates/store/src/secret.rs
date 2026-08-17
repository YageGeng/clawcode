//! Opaque credential types and storage boundary independent of OAuth semantics.

use std::fmt;

use protocol::McpServerId;
use serde::Serialize;

/// Stable credential key scoped by MCP Server and authorization subject.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey {
    server_id: McpServerId,
    subject: String,
}

impl SecretKey {
    /// Validates and creates one stable Server/subject credential key.
    pub fn new(
        server_id: McpServerId,
        subject: impl Into<String>,
    ) -> Result<Self, SecretStoreError> {
        let subject = subject.into();
        if subject.trim().is_empty() {
            return Err(SecretStoreError::InvalidKey(
                "Secret subject must not be empty".to_string(),
            ));
        }
        Ok(Self { server_id, subject })
    }

    /// Returns a deterministic filesystem-safe name without exposing the subject directly.
    pub(crate) fn file_name(&self) -> String {
        let source = format!("v1\0{}\0{}", self.server_id, self.subject);
        let mut encoded = String::with_capacity(source.len() * 2 + 7);
        for byte in source.bytes() {
            for nibble in [byte >> 4, byte & 0x0f] {
                let digit = match nibble {
                    0..=9 => b'0' + nibble,
                    _ => b'a' + (nibble - 10),
                };
                encoded.push(char::from(digit));
            }
        }
        encoded.push_str(".secret");
        encoded
    }
}

impl fmt::Debug for SecretKey {
    /// Exposes the routing Server while redacting the authorization subject.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretKey")
            .field("server_id", &self.server_id)
            .field("subject", &"<redacted>")
            .finish()
    }
}

/// Secret bytes whose ordinary formatting and serialization are always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    /// Creates a non-empty opaque Secret payload.
    pub fn new(value: Vec<u8>) -> Result<Self, SecretStoreError> {
        if value.is_empty() {
            return Err(SecretStoreError::InvalidValue);
        }
        Ok(Self(value))
    }

    /// Explicitly exposes bytes only to credential storage and authentication code.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    /// Prevents debug output from exposing credential bytes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue(<redacted>)")
    }
}

impl fmt::Display for SecretValue {
    /// Prevents user-facing formatting from exposing credential bytes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl Serialize for SecretValue {
    /// Serializes only a stable redaction marker; persistence uses the explicit byte boundary.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("<redacted>")
    }
}

/// Failures from Secret key validation or durable credential storage.
#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    /// Filesystem operation failed.
    #[error("Secret Store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Stored envelope is malformed, unsupported, or undecodable.
    #[error("Secret Store credential is corrupt: {0}")]
    Corrupt(String),
    /// Stable key construction rejected an empty authorization subject.
    #[error("invalid Secret key: {0}")]
    InvalidKey(String),
    /// Empty values cannot represent usable credentials.
    #[error("Secret value must not be empty")]
    InvalidValue,
}

/// Thread-safe credential persistence boundary consumed by authentication modules.
pub trait SecretStore: Send + Sync {
    /// Loads one Secret or reports that no credential exists.
    fn load(
        &self,
        key: &SecretKey,
    ) -> Result<Option<SecretValue>, SecretStoreError>;

    /// Atomically creates or replaces one Secret.
    fn store(
        &self,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretStoreError>;

    /// Deletes one Secret while treating an absent credential as success.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretStoreError>;
}
