use std::str::FromStr;

use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter, EnumString, IntoStaticStr};

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, EnumIter, AsRefStr, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    Raw,
    Sessions,
    Agent,
}

impl SlashCommand {
    /// Command string without the leading '/'.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// User-visible description.
    pub fn description(self) -> &'static str {
        match self {
            Self::Raw => "toggle raw scrollback mode for copy-friendly terminal output",
            Self::Sessions => "list recent sessions",
            Self::Agent => "switch between main agent and subagents",
        }
    }

    /// Whether this command supports inline args (e.g. `/raw on`).
    pub fn supports_inline_args(self) -> bool {
        matches!(self, Self::Raw | Self::Sessions)
    }

    /// Parse a submitted line into a SlashCommand.
    /// Returns `None` if the line is not a recognized slash command.
    pub fn parse_from_text(text: &str) -> Option<Self> {
        let (name, _rest, _rest_offset) = crate::command::prompt_args::parse_slash_name(text)?;
        Self::from_str(name).ok()
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter().map(|c| (c.command(), c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn parse_known_commands() {
        assert_eq!(SlashCommand::from_str("raw"), Ok(SlashCommand::Raw));
        assert_eq!(
            SlashCommand::from_str("sessions"),
            Ok(SlashCommand::Sessions)
        );
        assert_eq!(SlashCommand::from_str("agent"), Ok(SlashCommand::Agent));
    }

    #[test]
    fn parse_unknown_command() {
        SlashCommand::from_str("unknown").unwrap_err();
    }

    #[test]
    fn parse_from_text_with_slash() {
        assert_eq!(
            SlashCommand::parse_from_text("/sessions"),
            Some(SlashCommand::Sessions)
        );
        assert_eq!(
            SlashCommand::parse_from_text("/sessions 10"),
            Some(SlashCommand::Sessions)
        );
        assert_eq!(
            SlashCommand::parse_from_text("/agent"),
            Some(SlashCommand::Agent)
        );
    }

    #[test]
    fn parse_from_text_without_slash() {
        assert_eq!(SlashCommand::parse_from_text("sessions"), None);
    }

    #[test]
    fn command_names() {
        assert_eq!(SlashCommand::Raw.command(), "raw");
        assert_eq!(SlashCommand::Sessions.command(), "sessions");
        assert_eq!(SlashCommand::Agent.command(), "agent");
    }

    #[test]
    fn descriptions() {
        assert!(SlashCommand::Raw.description().contains("raw"));
        assert!(SlashCommand::Sessions.description().contains("session"));
        assert!(SlashCommand::Agent.description().contains("agent"));
    }

    #[test]
    fn built_in_commands_contains_all() {
        let commands = built_in_slash_commands();
        assert_eq!(commands.len(), 3);
        let names: Vec<&str> = commands.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"raw"));
        assert!(names.contains(&"sessions"));
        assert!(names.contains(&"agent"));
    }

    #[test]
    fn supports_inline_args() {
        assert!(SlashCommand::Raw.supports_inline_args());
        assert!(SlashCommand::Sessions.supports_inline_args());
        assert!(!SlashCommand::Agent.supports_inline_args());
    }
}
