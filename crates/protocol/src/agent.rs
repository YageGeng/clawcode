//! Codex multi-agent v2 addressing, nicknames, and inter-agent mail.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    ContentBlock, ExtensionId, ExtensionMessage, ProductIdentity, ScalarError,
};

/// Tool that spawns one named child agent.
pub const SPAWN_AGENT_TOOL: &str = "spawn_agent";
/// Tool that queues a message without waking the recipient.
pub const SEND_MESSAGE_TOOL: &str = "send_message";
/// Tool that queues a task and wakes an idle child.
pub const FOLLOWUP_TASK_TOOL: &str = "followup_task";
/// Tool that waits for mailbox activity.
pub const WAIT_AGENT_TOOL: &str = "wait_agent";
/// Tool that lists the agent tree.
pub const LIST_AGENTS_TOOL: &str = "list_agents";
/// Tool that cancels a child's current turn.
pub const INTERRUPT_AGENT_TOOL: &str = "interrupt_agent";
/// Extension custom type of persisted inter-agent mail.
pub const AGENT_MESSAGE_TYPE: &str = "agent_message";
/// Extension custom type of the hidden `<subagents>` roster note.
pub const SUBAGENT_ROSTER_TYPE: &str = "subagent_roster";
/// Extension custom type of the hidden note that tells a child who it is.
pub const SUBAGENT_ROLE_TYPE: &str = "subagent_role";

/// Codex nickname pool, one name per line.
const AGENT_NAMES: &str = include_str!("../assets/agent_names.txt");

/// One validated `agent_name` path segment (Codex `validate_agent_name`).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl TryFrom<String> for AgentName {
    type Error = String;

    /// Applies Codex's rules and wording: lowercase letters, digits, and
    /// underscores, never empty, never a reserved segment.
    fn try_from(name: String) -> Result<Self, Self::Error> {
        if name.is_empty() {
            return Err("agent_name must not be empty".to_string());
        }
        if name == AgentPath::ROOT_SEGMENT || name == "." || name == ".." {
            return Err(format!("agent_name `{name}` is reserved"));
        }
        if name.contains('/') {
            return Err("agent_name must not contain `/`".to_string());
        }
        if !name.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'
        }) {
            return Err(
                "agent_name must use only lowercase letters, digits, and underscores"
                    .to_string(),
            );
        }
        Ok(Self(name))
    }
}

impl TryFrom<&str> for AgentName {
    type Error = String;

    /// Borrowing form of the owned name parser.
    fn try_from(name: &str) -> Result<Self, Self::Error> {
        Self::try_from(name.to_string())
    }
}

impl From<AgentName> for String {
    /// Serializes the bare name.
    fn from(name: AgentName) -> Self {
        name.0
    }
}

impl AsRef<str> for AgentName {
    /// Borrows the bare name.
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for AgentName {
    /// Lets maps keyed by name be queried with a path's last segment.
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    /// Writes the bare name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Canonical agent address: `/root` or `/root/<agent_name>` (Codex AgentPath, one level deep).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentPath(String);

impl AgentPath {
    /// Address of the owner agent.
    pub const ROOT: &str = "/root";
    const ROOT_SEGMENT: &str = "root";

    /// Returns the owner agent's address.
    #[must_use]
    pub fn root() -> Self {
        Self(Self::ROOT.to_string())
    }

    /// Returns the address of one named child of `/root`.
    #[must_use]
    pub fn child(name: &AgentName) -> Self {
        Self(format!("{}/{name}", Self::ROOT))
    }

    /// Returns the last segment, or `root` for the owner.
    #[must_use]
    pub fn name(&self) -> &str {
        self.0
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(Self::ROOT_SEGMENT)
    }

    /// Reports whether this is the owner's address.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == Self::ROOT
    }

    /// Resolves a tool target like Codex: `/`-prefixed paths are absolute,
    /// anything else is one child name below `/root`.
    pub fn resolve(reference: &str) -> Result<Self, String> {
        if reference.starts_with('/') {
            return Self::try_from(reference);
        }
        Ok(Self::child(&AgentName::try_from(reference)?))
    }
}

impl TryFrom<String> for AgentPath {
    type Error = String;

    /// Accepts `/root` and `/root/<agent_name>` only.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == Self::ROOT {
            return Ok(Self(value));
        }
        let Some(rest) = value.strip_prefix("/root/") else {
            return Err(
                "absolute agent paths must start with `/root`".to_string()
            );
        };
        if rest.contains('/') {
            return Err(format!(
                "agent path `{value}` is deeper than one level"
            ));
        }
        AgentName::try_from(rest)?;
        Ok(Self(value))
    }
}

impl TryFrom<&str> for AgentPath {
    type Error = String;

    /// Borrowing form of the owned path parser.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_string())
    }
}

impl From<AgentPath> for String {
    /// Serializes the canonical path text.
    fn from(value: AgentPath) -> Self {
        value.0
    }
}

impl AsRef<str> for AgentPath {
    /// Borrows the canonical path text.
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentPath {
    /// Writes the canonical path text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// User-facing child nickname drawn from Codex's name pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNickname(String);

impl AgentNickname {
    /// Picks the `index`-th unused name of the first round that still has one.
    ///
    /// Round 0 uses bare names; round `r` appends Codex's ordinal suffix
    /// (`Euclid the 2nd`), so allocation never runs out and stays a pure
    /// function of `used` and `index`.
    #[must_use]
    pub fn allocate(used: &HashSet<String>, index: usize) -> Self {
        let names = AGENT_NAMES
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        let mut round = 0_usize;
        loop {
            let available = names
                .iter()
                .map(|name| Self::format(name, round))
                .filter(|name| !used.contains(name))
                .collect::<Vec<_>>();
            if let Some(len) = std::num::NonZeroUsize::new(available.len())
                && let Some(name) = available.into_iter().nth(index % len)
            {
                return Self(name);
            }
            round = round.saturating_add(1);
        }
    }

    /// Formats one pool name for an allocation round like Codex `format_agent_nickname`.
    fn format(name: &str, round: usize) -> String {
        if round == 0 {
            return name.to_string();
        }
        let value = round.saturating_add(1);
        let suffix = match value % 100 {
            11..=13 => "th",
            _ => match value % 10 {
                1 => "st",
                2 => "nd",
                3 => "rd",
                _ => "th",
            },
        };
        format!("{name} the {value}{suffix}")
    }

    /// Returns the nickname text.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// Codex InterAgentMessageType subset used here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    /// Queue-only message.
    Message,
    /// Task that wakes an idle recipient or extends its running turn.
    NewTask,
    /// A child's terminal answer for its owner.
    Result,
}

impl MailKind {
    /// Returns the wire and model-visible name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::NewTask => "new_task",
            Self::Result => "result",
        }
    }

    /// Mirrors Codex `trigger_turn`: only new tasks wake or extend a Run.
    #[must_use]
    pub const fn triggers_turn(self) -> bool {
        matches!(self, Self::NewTask)
    }
}

/// One inter-agent message; the only owner of its model-facing text.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct AgentMail {
    /// Sending agent.
    pub author: AgentPath,
    /// Receiving agent.
    pub recipient: AgentPath,
    /// Delivery semantics.
    pub kind: MailKind,
    /// Message body.
    pub payload: String,
}

impl AgentMail {
    /// Renders Codex `InterAgentMessage` text exactly as the model reads it.
    #[must_use]
    pub fn model_text(&self) -> String {
        format!(
            "Message Type: {}\nTask name: {}\nSender: {}\nPayload:\n{}",
            self.kind.as_str(),
            self.recipient,
            self.author,
            self.payload
        )
    }
}

impl TryFrom<AgentMail> for ExtensionMessage {
    type Error = ScalarError;

    /// Builds the visible, context-included Extension message that persists one mail.
    fn try_from(mail: AgentMail) -> Result<Self, Self::Error> {
        let details = serde_json::json!({
            "kind": mail.kind.as_str(),
            "author": mail.author.as_ref(),
            "recipient": mail.recipient.as_ref(),
        });
        Ok(Self::builder()
            .extension_id(ExtensionId::try_from(ProductIdentity::SLUG)?)
            .custom_type(AGENT_MESSAGE_TYPE.to_string())
            .blocks(vec![ContentBlock::Text {
                text: mail.model_text(),
            }])
            .display(true)
            .include_in_context(true)
            .details(details)
            .build())
    }
}
