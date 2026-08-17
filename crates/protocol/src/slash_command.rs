use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::ContentBlock;

/// Runtime owner of one command advertised to protocol clients.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SlashCommandSource {
    /// Command implemented directly by the Kernel.
    Builtin,
    /// Command handled by a registered Extension.
    Extension,
    /// Command expanding a discovered Skill document.
    Skill,
    /// Command expanding a discovered Prompt Template.
    PromptTemplate,
}

/// Relationship between one advertised name and its canonical command name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlashCommandAliasKind {
    /// Name is the canonical executable command name.
    Canonical,
    /// Name is an unambiguous shorthand for a qualified command.
    Short,
}

/// Complete command metadata projected into ACP Available Commands.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "snake_case")]
pub struct SlashCommandDefinition {
    /// Slash-command name without the leading slash.
    pub name: String,
    /// Human-readable command purpose.
    pub description: String,
    /// Optional argument shape displayed by protocol clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub argument_hint: Option<String>,
    /// Runtime owner controlling execution precedence.
    pub source: SlashCommandSource,
    /// Canonical qualified name when this definition is an alias.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub qualified_name: Option<String>,
    /// Whether the advertised name is canonical or shorthand.
    pub alias_kind: SlashCommandAliasKind,
}

/// Parsed leading-slash input with its original text preserved for replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandInvocation {
    /// Exact user-authored command input.
    pub original: String,
    /// Command name without the leading slash.
    pub name: String,
    /// Argument tail after removing exactly one U+0020 delimiter.
    pub arguments: String,
}

/// Failures produced before a Slash Command can be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SlashCommandParseError {
    /// Input does not begin with a slash.
    #[error("input is not a Slash Command")]
    NotCommand,
    /// Input contains a slash without a command name.
    #[error("Slash Command name cannot be empty")]
    EmptyName,
}

impl TryFrom<&str> for SlashCommandInvocation {
    type Error = SlashCommandParseError;

    /// Parses a leading slash and the first U+0020 delimiter without tokenizing arguments.
    fn try_from(input: &str) -> Result<Self, Self::Error> {
        let command = input
            .strip_prefix('/')
            .ok_or(SlashCommandParseError::NotCommand)?;
        let (name, arguments) = command
            .split_once(' ')
            .map_or((command, ""), |(name, arguments)| (name, arguments));
        if name.is_empty() {
            return Err(SlashCommandParseError::EmptyName);
        }
        Ok(Self {
            original: input.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        })
    }
}

/// One Skill or Prompt Template expansion with separate display and model projections.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SlashCommandExpansion {
    /// Original invocation displayed to users and retained for replay.
    pub invocation: SlashCommandInvocation,
    /// Resource type that expanded the invocation.
    pub source: SlashCommandSource,
    /// Frozen content sent to the model and restored after Session resume.
    pub model_blocks: Vec<ContentBlock>,
}

/// Terminal state of one directly handled Slash Command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlashCommandStatus {
    /// Command completed its intended behavior.
    Succeeded,
    /// Command was rejected or failed during execution.
    Failed,
}

/// Displayable output from a directly handled Slash Command.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SlashCommandOutput {
    /// Executed command name without the leading slash.
    pub command: String,
    /// Runtime owner that produced the output.
    pub source: SlashCommandSource,
    /// Terminal command status.
    pub status: SlashCommandStatus,
    /// Ordered output represented with shared content blocks.
    pub blocks: Vec<ContentBlock>,
}

/// Persistable input or output belonging to a directly handled Slash Command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SlashCommandMessage {
    /// Exact user-authored invocation plus its resolved runtime owner.
    Invocation {
        /// Original invocation retained for display and replay.
        invocation: SlashCommandInvocation,
        /// Runtime owner selected by command precedence.
        source: SlashCommandSource,
    },
    /// Server-authored terminal output.
    Output(SlashCommandOutput),
}

impl SlashCommandMessage {
    /// Returns the user-visible text represented by this command message.
    #[must_use]
    pub fn text_content(&self) -> Option<String> {
        match self {
            Self::Invocation { invocation, .. } => {
                Some(invocation.original.clone())
            }
            Self::Output(output) => {
                let text = output
                    .blocks
                    .iter()
                    .filter_map(ContentBlock::text)
                    .collect::<Vec<_>>()
                    .join("");
                (!text.is_empty()).then_some(text)
            }
        }
    }
}

/// Stable failures returned by recognized commands without becoming transport errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlashCommandError {
    /// Arguments do not satisfy the command contract.
    InvalidArguments {
        /// Recognized command name.
        command: String,
        /// Stable usage text shown to clients.
        usage: String,
    },
    /// Session state prevents immediate command execution.
    Busy {
        /// Recognized command name.
        command: String,
    },
    /// Command execution failed after successful resolution.
    ExecutionFailed {
        /// Recognized command name.
        command: String,
    },
    /// More than one Extension owns the requested shorthand.
    Ambiguous {
        /// Ambiguous shorthand.
        command: String,
        /// Executable qualified alternatives.
        candidates: Vec<String>,
    },
}

impl Display for SlashCommandError {
    /// Renders stable client-facing failures without internal error details.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArguments { command, usage } => {
                write!(formatter, "invalid arguments for /{command}: {usage}")
            }
            Self::Busy { command } => {
                write!(
                    formatter,
                    "/{command} cannot run while the Session is busy"
                )
            }
            Self::ExecutionFailed { command } => {
                write!(formatter, "/{command} failed")
            }
            Self::Ambiguous {
                command,
                candidates,
            } => {
                write!(formatter, "/{command} is ambiguous")?;
                if !candidates.is_empty() {
                    write!(formatter, "; use ")?;
                    for (index, candidate) in candidates.iter().enumerate() {
                        if index > 0 {
                            write!(formatter, " or ")?;
                        }
                        write!(formatter, "/{candidate}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SlashCommandError {}

/// Typed result returned by one stage of Kernel Slash Command routing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SlashCommandOutcome {
    /// Direct command completed without entering the normal Agent loop.
    Handled {
        /// Optional displayable terminal output.
        output: Option<SlashCommandOutput>,
    },
    /// Skill or Prompt Template expanded into frozen model content.
    ExpandedPrompt(SlashCommandExpansion),
    /// No registered command matched and the original input must continue.
    NotFound,
    /// A recognized command rejected or failed the invocation.
    Rejected(SlashCommandError),
}
