//! Catalog rendering: converts enabled [`SkillMetadata`] into the
//! `skills_xml` text block injected into the system prompt.

use crate::SkillMetadata;

/// Render the list of enabled skills as an XML block.
///
/// Format follows the OpenCode reference implementation:
/// `<available_skills>` wraps `<skill>` entries with `<name>`,
/// `<description>`, `<location>` (file:// URL), and `<scope>` children.
///
/// Returns `None` when `skills` is empty (so the caller can leave
/// `skills_xml` unset and avoid injecting an empty section).
pub(crate) fn render_catalog(skills: &[&SkillMetadata]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push("<available_skills>".to_string());

    for skill in skills {
        let scope_tag = match skill.scope {
            crate::SkillScope::Repo => "repo",
            crate::SkillScope::User => "user",
        };
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", skill.name));
        lines.push(format!(
            "    <description>{}</description>",
            skill.description
        ));
        // Use file:// URL for location, consistent with OpenCode.
        lines.push(format!(
            "    <location>file://{}</location>",
            skill.path.display()
        ));
        lines.push(format!("    <scope>{}</scope>", scope_tag));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());

    // Intro and usage instructions, matching OpenCode's system prompt style.
    lines.push(String::new());
    lines.push(
        "Skills provide specialized instructions and workflows for specific tasks.".to_string(),
    );
    lines.push(
        "Use the skill tool to load a skill when a task matches its description.".to_string(),
    );
    lines.push(String::new());
    lines.push("### How to use skills".to_string());
    lines.push(
        "- Discovery: The list above shows available skills (name + description + file path)."
            .to_string(),
    );
    lines.push(
        "- Trigger: If the user names a skill (with `$SkillName` or plain text) OR the task clearly matches a skill's description, use that skill for that turn.".to_string(),
    );
    lines.push(
        "- Usage: Use the skill tool to load the skill's full instructions and resources into the conversation.".to_string(),
    );
    lines.push(
        "- Paths: Relative paths in SKILL.md resolve relative to the skill directory containing SKILL.md.".to_string(),
    );

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn empty_catalog_returns_none() {
        assert!(render_catalog(&[]).is_none());
    }

    #[test]
    fn renders_single_skill() {
        let skill = SkillMetadata {
            name: "demo".to_string(),
            description: "A demo skill".to_string(),
            path: PathBuf::from("/tmp/demo/SKILL.md"),
            scope: crate::SkillScope::Repo,
        };
        let catalog = render_catalog(&[&skill]).unwrap();
        assert!(catalog.contains("<available_skills>"));
        assert!(catalog.contains("<name>demo</name>"));
        assert!(catalog.contains("<description>A demo skill</description>"));
        assert!(catalog.contains("file:///tmp/demo/SKILL.md"));
        assert!(catalog.contains("<scope>repo</scope>"));
        assert!(catalog.contains("### How to use skills"));
    }

    #[test]
    fn scope_tags() {
        let repo_skill = SkillMetadata {
            name: "r".to_string(),
            description: String::new(),
            path: PathBuf::from("/r/SKILL.md"),
            scope: crate::SkillScope::Repo,
        };
        let user_skill = SkillMetadata {
            name: "u".to_string(),
            description: String::new(),
            path: PathBuf::from("/u/SKILL.md"),
            scope: crate::SkillScope::User,
        };
        let catalog = render_catalog(&[&repo_skill, &user_skill]).unwrap();
        assert!(catalog.contains("<scope>repo</scope>"));
        assert!(catalog.contains("<scope>user</scope>"));
    }
}
