use serde::{Deserialize, Serialize};

/// Runtime category of one command advertised to ACP clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailableAgentCommandKind {
    /// Command executed directly by an Extension.
    Extension,
    /// Command expanding a discovered Skill document.
    Skill,
    /// Command expanding a discovered Prompt Template.
    PromptTemplate,
}

/// Complete command metadata projected into ACP available commands.
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
pub struct AvailableAgentCommand {
    /// Slash-command name without the leading slash.
    pub name: String,
    /// Human-readable command purpose.
    pub description: String,
    /// Optional argument shape displayed by protocol clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub argument_hint: Option<String>,
    /// Runtime category controlling execution precedence.
    pub kind: AvailableAgentCommandKind,
}
