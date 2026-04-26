use std::path::PathBuf;

use llm::completion::Message;

/// Runtime-facing structured user input for one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserInput {
    /// Plain text that becomes a prompt-visible user message.
    Text { text: String },
    /// Structured skill selection metadata that does not become a normal user message.
    Skill { name: String, path: PathBuf },
}

impl UserInput {
    /// Builds a plain text input that becomes a prompt-visible user message.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Builds a structured skill selection that resolves by exact skill path.
    pub fn skill(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::Skill {
            name: name.into(),
            path: path.into(),
        }
    }
}

/// Renders inputs into a durable display string for events and turn history.
pub fn user_inputs_display_text(inputs: &[UserInput]) -> String {
    inputs
        .iter()
        .map(display_one_input)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Converts only text inputs into prompt-visible model messages.
pub fn user_inputs_to_messages(inputs: &[UserInput]) -> Vec<Message> {
    inputs
        .iter()
        .filter_map(|input| match input {
            UserInput::Text { text } => Some(Message::user(text.clone())),
            UserInput::Skill { .. } => None,
        })
        .collect()
}

/// Converts kernel user inputs into skills-crate inputs for mention collection.
pub fn user_inputs_to_skill_inputs(inputs: &[UserInput]) -> Vec<skills::SkillInput> {
    inputs
        .iter()
        .map(|input| match input {
            UserInput::Text { text } => skills::SkillInput::text(text.clone()),
            UserInput::Skill { name, path } => {
                skills::SkillInput::skill(name.clone(), path.clone())
            }
        })
        .collect()
}

/// Renders one input into the stable text form used for events and persisted history labels.
fn display_one_input(input: &UserInput) -> String {
    match input {
        UserInput::Text { text } => text.clone(),
        UserInput::Skill { name, path } => format!("[skill:{name}]({})", path.display()),
    }
}
