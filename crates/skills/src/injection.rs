use llm::completion::Message;
use snafu::ResultExt;

use crate::error::{IoSnafu, Result};
use crate::model::SkillMetadata;

/// Reads selected skill files and returns prompt-visible instruction messages.
pub async fn build_skill_injections(skills: &[SkillMetadata]) -> Result<Vec<Message>> {
    let mut messages = Vec::with_capacity(skills.len());

    for skill in skills {
        let contents = tokio::fs::read_to_string(&skill.path)
            .await
            .context(IoSnafu {
                stage: "read-skill-injection".to_string(),
                path: skill.path.clone(),
            })?;

        messages.push(Message::user(format!(
            "<skill_instructions name=\"{}\" path=\"{}\">\n{}\n</skill_instructions>",
            escape_attr(&skill.name),
            escape_attr(&skill.path.to_string_lossy()),
            contents
        )));
    }

    Ok(messages)
}

/// Escapes XML-sensitive characters used in instruction tag attributes.
fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
