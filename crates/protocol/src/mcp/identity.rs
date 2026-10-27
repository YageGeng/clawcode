use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// MCP revisions accepted by the Pi-compatible initialize lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum McpProtocolVersion {
    /// Legacy initialize lifecycle published on 2025-11-25.
    #[serde(rename = "2025-11-25")]
    V2025_11_25,
    /// Initialize revision published on 2025-06-18.
    #[serde(rename = "2025-06-18")]
    V2025_06_18,
    /// Initialize revision published on 2025-03-26.
    #[serde(rename = "2025-03-26")]
    V2025_03_26,
    /// Initialize revision published on 2024-11-05.
    #[serde(rename = "2024-11-05")]
    V2024_11_05,
}

/// Reports an unsupported exact MCP protocol revision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported MCP protocol version: {value}")]
pub struct McpProtocolVersionError {
    /// Unsupported wire version supplied by configuration or negotiation.
    pub value: String,
}

impl FromStr for McpProtocolVersion {
    type Err = McpProtocolVersionError;

    /// Parses one exact supported protocol revision without fallback.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "2025-11-25" => Ok(Self::V2025_11_25),
            "2025-06-18" => Ok(Self::V2025_06_18),
            "2025-03-26" => Ok(Self::V2025_03_26),
            "2024-11-05" => Ok(Self::V2024_11_05),
            _ => Err(McpProtocolVersionError {
                value: value.to_string(),
            }),
        }
    }
}

impl fmt::Display for McpProtocolVersion {
    /// Writes the exact protocol version used on the MCP wire.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V2025_11_25 => formatter.write_str("2025-11-25"),
            Self::V2025_06_18 => formatter.write_str("2025-06-18"),
            Self::V2025_03_26 => formatter.write_str("2025-03-26"),
            Self::V2024_11_05 => formatter.write_str("2024-11-05"),
        }
    }
}

impl McpProtocolVersion {
    /// Returns the accepted revisions in preferred order without a second registry.
    pub const fn supported() -> &'static [Self] {
        &[
            Self::V2025_11_25,
            Self::V2025_06_18,
            Self::V2025_03_26,
            Self::V2024_11_05,
        ]
    }

    /// Reports whether this revision supports form elicitation.
    pub const fn supports_elicitation(self) -> bool {
        matches!(self, Self::V2025_11_25 | Self::V2025_06_18)
    }
}

/// Requested initialization preference; every supported server selection remains acceptable.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub enum McpProtocolPolicy {
    #[default]
    Auto,
    Preferred(McpProtocolVersion),
}

impl McpProtocolPolicy {
    /// Selects the initial request revision without prescribing a retry sequence.
    pub const fn requested_version(self) -> McpProtocolVersion {
        match self {
            Self::Auto => McpProtocolVersion::V2025_11_25,
            Self::Preferred(version) => version,
        }
    }
}

impl FromStr for McpProtocolPolicy {
    type Err = McpProtocolVersionError;
    /// Parses automatic negotiation or a supported request preference.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "auto" {
            Ok(Self::Auto)
        } else {
            value.parse().map(Self::Preferred)
        }
    }
}

impl TryFrom<String> for McpProtocolPolicy {
    type Error = McpProtocolVersionError;
    /// Validates a serialized protocol preference at the boundary.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl fmt::Display for McpProtocolPolicy {
    /// Formats the same scalar accepted by TOML configuration.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => formatter.write_str("auto"),
            Self::Preferred(version) => version.fmt(formatter),
        }
    }
}

impl From<McpProtocolPolicy> for String {
    /// Serializes a preference without confusing it with the negotiated version.
    fn from(value: McpProtocolPolicy) -> Self {
        value.to_string()
    }
}

/// Transport selected for one configured MCP Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpTransportKind {
    /// Child process communicating over standard input and output.
    Stdio,
    /// MCP Streamable HTTP client transport.
    StreamableHttp,
}

/// Validation failures for stable MCP Server identifiers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpIdentityError {
    /// Server identifiers must contain at least one character.
    #[error("MCP server id must not be empty")]
    Empty,
    /// Server identifiers accept only characters safe in model-facing Tool names.
    #[error("MCP server id contains a namespace-unsafe character: {0}")]
    InvalidCharacter(char),
    /// The separator is reserved for the generated Tool namespace.
    #[error("MCP server id must not contain the reserved '__' separator")]
    ReservedSeparator,
}

/// Validated MCP Server identifier used in routing and public namespacing.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct McpServerId(String);

impl McpServerId {
    /// Returns the validated Server identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for McpServerId {
    type Error = McpIdentityError;

    /// Parses a namespace-safe Server identifier without normalization.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err(McpIdentityError::Empty);
        }
        if value.contains("__") {
            return Err(McpIdentityError::ReservedSeparator);
        }
        if let Some(character) = value.chars().find(|character| {
            !character.is_ascii_alphanumeric()
                && *character != '-'
                && *character != '_'
        }) {
            return Err(McpIdentityError::InvalidCharacter(character));
        }
        Ok(Self(value))
    }
}

impl TryFrom<&str> for McpServerId {
    type Error = McpIdentityError;

    /// Copies and parses a borrowed MCP Server identifier.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_string())
    }
}

impl From<McpServerId> for String {
    /// Consumes a Server identifier into its wire value.
    fn from(value: McpServerId) -> Self {
        value.0
    }
}

impl AsRef<str> for McpServerId {
    /// Borrows the Server identifier as a string slice.
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for McpServerId {
    /// Writes the Server identifier without decoration.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured reference to one remotely discovered MCP Tool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolRef {
    /// Server that owns the Tool.
    pub server_id: McpServerId,
    /// Original remote Tool name.
    pub remote_name: String,
}

impl McpToolRef {
    /// Generates the stable model-facing Tool name without losing the routing reference.
    #[must_use]
    pub fn public_name(&self) -> String {
        use sha2::{Digest, Sha256};

        let base = format!("mcp__{}__{}", self.server_id, self.remote_name);
        if base.len() <= 64
            && base.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
            })
        {
            return base;
        }
        // Length framing keeps distinct routing identities distinct before hashing.
        let mut digest = Sha256::new();
        digest.update(b"clawcode-mcp-tool-v1\0");
        for part in [self.server_id.as_str(), self.remote_name.as_str()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        let prefix: String = base
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric()
                    || matches!(character, '_' | '-')
                {
                    character
                } else {
                    '_'
                }
            })
            .take(47)
            .collect();
        let hash = digest.finalize();
        let suffix: String = hash
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("{prefix}_{suffix}")
    }
}

/// Structured reference to one remotely discovered MCP Prompt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptRef {
    /// Server that owns the Prompt.
    pub server_id: McpServerId,
    /// Original remote Prompt name.
    pub remote_name: String,
}

/// Structured reference to one remotely discovered MCP Resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceRef {
    /// Server that owns the Resource.
    pub server_id: McpServerId,
    /// Original remote Resource URI.
    pub remote_uri: String,
}
