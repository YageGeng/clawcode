use std::fmt;
use std::path::{Path, PathBuf};

/// Absolute path supplied through the product configuration override variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOverridePath(PathBuf);

impl TryFrom<PathBuf> for ConfigOverridePath {
    type Error = ConfigOverridePathError;

    /// Rejects relative overrides whose meaning would differ across build and runtime processes.
    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(ConfigOverridePathError::Relative(path))
        }
    }
}

impl AsRef<Path> for ConfigOverridePath {
    /// Borrows the validated absolute path.
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl From<ConfigOverridePath> for PathBuf {
    /// Consumes the validated override into its filesystem representation.
    fn from(path: ConfigOverridePath) -> Self {
        path.0
    }
}

/// Validation failures for an explicit configuration path override.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigOverridePathError {
    /// Relative paths have no stable base across Cargo and the running application.
    #[error("configuration override must be an absolute path: {0}")]
    Relative(PathBuf),
}

/// Centralizes names that must change together when the product is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductIdentity;

impl ProductIdentity {
    /// Human-readable product name.
    pub const NAME: &str = "Clawcode";

    /// Lowercase product identifier used in machine-readable contexts.
    pub const SLUG: &str = "clawcode";

    /// Namespace used by ACP extension methods and metadata.
    pub const ACP_NAMESPACE: &str = "clawcode";

    /// ACP v2 custom session-update discriminator for runtime events.
    pub const ACP_EVENT_UPDATE: &str = "_clawcode/event";

    /// ACP v2 extension stop reason used when foreground work fails.
    pub const ACP_ERROR_STOP_REASON: &str = "_clawcode/error";

    /// Product metadata key carrying Slash Command source and terminal state.
    pub const ACP_SLASH_COMMAND_METADATA: &str = "slashCommand";

    /// ACP v2 custom content discriminator for structured MCP Tool output.
    pub const ACP_STRUCTURED_CONTENT_BLOCK: &str =
        "_clawcode/structured_content";

    /// ACP v2 extension notification for an updated MCP Session snapshot revision.
    pub const ACP_MCP_UPDATE_NOTIFICATION: &str = "_clawcode/mcp/update";

    /// ACP extension notification for a transient terminal change.
    pub const ACP_TERMINAL_UPDATE_NOTIFICATION: &str =
        "_clawcode/terminal/update";

    /// Prefix used by product-specific environment variables.
    pub const CONFIG_ENV_PREFIX: &str = "CLAW_";

    /// Environment variable exposing the active session to local bash tools.
    pub const SESSION_ID_ENV: &str = "CLAW_SESSION_ID";

    /// Environment variable exposing the active Turn to local bash tools.
    pub const TURN_ID_ENV: &str = "CLAW_TURN_ID";

    /// Environment variable that overrides the default config search path.
    pub const CONFIG_PATH_ENV: &str = "CLAW_CONFIG";

    /// Default TOML configuration filename.
    pub const CONFIG_FILE_NAME: &str = "claw.toml";

    /// Default product configuration directory name.
    pub const CONFIG_DIR_NAME: &str = "clawcode";

    /// Filename used inside the user-level configuration directory.
    pub const USER_CONFIG_FILE_NAME: &str = "config.toml";

    /// Tracing target used for provider factory diagnostics.
    pub const TRACING_FACTORY_TARGET: &str = "clawcode::factory";

    /// Tracing target used for provider request and stream diagnostics.
    pub const TRACING_COMPLETIONS_TARGET: &str = "clawcode::completions";

    /// Prefix used for native ChatGPT authentication temporary files.
    pub const CHATGPT_AUTH_TEMP_PREFIX: &str = "clawcode-chatgpt-auth";

    /// Prefix used for complete truncated bash-output temporary files.
    pub const BASH_OUTPUT_TEMP_PREFIX: &str = "clawcode-bash-";

    /// Default same-origin route used by ACP HTTP, SSE, and WebSocket transports.
    pub const DEFAULT_ACP_PATH: &str = "/acp";

    /// Same-origin route that supplies immutable WebUI startup configuration.
    pub const UI_BOOTSTRAP_PATH: &str = "/api/ui/bootstrap";
}

/// Identifies every product-specific ACP extension method without raw strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcpExtensionMethod {
    /// Starts a follow-up turn.
    FollowUp,
    /// Reads the persisted session tree.
    Tree,
    /// Moves the active session cursor.
    Navigate,
    /// Creates a branch from a selected entry.
    Branch,
    /// Forks a session into a new persisted session.
    Fork,
    /// Compacts the active context.
    Compact,
    /// Reads queued steering and follow-up messages.
    PendingMessages,
    /// Removes one queued message by its stable queue identifier.
    PendingMessageRemove,
    /// Clears queued messages.
    ClearQueue,
    /// Changes the persisted display title for one session.
    SessionRename,
    /// Reads whether one Session currently owns an active Run.
    SessionRuntime,
    /// Lists retained terminals for one Session.
    TerminalList,
    /// Terminates one retained terminal.
    TerminalTerminate,
    /// Cleans every retained terminal for one Session.
    TerminalClean,
    /// Invokes a discovered skill explicitly.
    InvokeSkill,
    /// Lists effective discovered skills without returning their bodies.
    SkillList,
    /// Reads immutable MCP connection and tool status for one session.
    McpStatus,
    /// Explicitly reconnects one configured MCP Server.
    McpReconnect,
    /// Retrieves one discovered MCP Prompt.
    McpPromptGet,
    /// Reads one discovered MCP Resource.
    McpResourceRead,
    /// Completes one MCP Prompt or Resource Template argument.
    McpComplete,
    /// Continues one pending MCP OAuth browser round.
    McpOAuthContinue,
    /// Lists pending Turn-scoped MCP elicitations for one Session.
    McpElicitationList,
    /// Resolves one pending Turn-scoped MCP elicitation.
    McpElicitationRespond,
    /// Dispatches a registered extension command.
    ExtensionCommand,
    /// Executes a user-authored shell command entirely on the server.
    UserBash,
}

impl fmt::Display for AcpExtensionMethod {
    /// Renders the ACP v2 extension method with the shared product namespace.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AcpExtensionMethod {
    /// Returns the static JSON-RPC method name for registration and dispatch.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FollowUp => "_clawcode/session/follow_up",
            Self::Tree => "_clawcode/session/tree",
            Self::Navigate => "_clawcode/session/navigate",
            Self::Branch => "_clawcode/session/branch",
            Self::Fork => "_clawcode/session/fork",
            Self::Compact => "_clawcode/session/compact",
            Self::PendingMessages => "_clawcode/session/pending_messages",
            Self::PendingMessageRemove => {
                "_clawcode/session/pending_message/remove"
            }
            Self::ClearQueue => "_clawcode/session/clear_queue",
            Self::SessionRename => "_clawcode/session/rename",
            Self::SessionRuntime => "_clawcode/session/runtime",
            Self::TerminalList => "_clawcode/terminal/list",
            Self::TerminalTerminate => "_clawcode/terminal/terminate",
            Self::TerminalClean => "_clawcode/terminal/clean",
            Self::InvokeSkill => "_clawcode/skill/invoke",
            Self::SkillList => "_clawcode/skill/list",
            Self::McpStatus => "_clawcode/mcp/status",
            Self::McpReconnect => "_clawcode/mcp/reconnect",
            Self::McpPromptGet => "_clawcode/mcp/prompt/get",
            Self::McpResourceRead => "_clawcode/mcp/resource/read",
            Self::McpComplete => "_clawcode/mcp/complete",
            Self::McpOAuthContinue => "_clawcode/mcp/oauth/continue",
            Self::McpElicitationList => "_clawcode/mcp/elicitation/list",
            Self::McpElicitationRespond => "_clawcode/mcp/elicitation/respond",
            Self::ExtensionCommand => "_clawcode/extension/command",
            Self::UserBash => "_clawcode/session/bash",
        }
    }

    /// Parses one product-scoped ACP extension method without accepting foreign namespaces.
    pub fn parse(method: &str) -> Option<Self> {
        [
            Self::FollowUp,
            Self::Tree,
            Self::Navigate,
            Self::Branch,
            Self::Fork,
            Self::Compact,
            Self::PendingMessages,
            Self::PendingMessageRemove,
            Self::ClearQueue,
            Self::SessionRename,
            Self::SessionRuntime,
            Self::TerminalList,
            Self::TerminalTerminate,
            Self::TerminalClean,
            Self::InvokeSkill,
            Self::SkillList,
            Self::McpStatus,
            Self::McpReconnect,
            Self::McpPromptGet,
            Self::McpResourceRead,
            Self::McpComplete,
            Self::McpOAuthContinue,
            Self::McpElicitationList,
            Self::McpElicitationRespond,
            Self::ExtensionCommand,
            Self::UserBash,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == method)
    }
}
