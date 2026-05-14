//! Catalog rendering: converts enabled [`SkillMetadata`] into the
//! `skills_xml` text block injected into the system prompt.

use crate::SkillMetadata;

/// Render the list of enabled skills as a Markdown block.
///
/// Returns `None` when `skills` is empty (so the caller can leave
/// `skills_xml` unset and avoid injecting an empty section).
pub(crate) fn render_catalog(skills: &[&SkillMetadata]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut lines: Vec<&str> = Vec::new();
    lines.push("## Skills");
    lines.push("### Available skills");

    // Collect formatted skill lines as owned Strings since we need to
    // interpolate path.display() and variable-length fields.
    let mut skill_lines: Vec<String> = Vec::with_capacity(skills.len());
    for skill in skills {
        let scope_tag = match skill.scope {
            crate::SkillScope::Repo => "repo",
            crate::SkillScope::User => "user",
        };
        skill_lines.push(format!(
            "- `${}` ({}): {}. File: {}",
            skill.name,
            scope_tag,
            skill.description,
            skill.path.display(),
        ));
    }

    for line in &skill_lines {
        lines.push(line.as_str());
    }

    lines.push("");
    lines.push("### How to use skills");
    lines.push(
        "- Discovery: The list above shows available skills (name + description + file path).",
    );
    lines.push(
        "- Trigger: If the user names a skill (with `$SkillName` or plain text) OR the task clearly matches a skill's description, use that skill for that turn.",
    );
    lines.push(
        "- Usage: After deciding to use a skill, open its SKILL.md with the Read tool. Read only enough to follow the workflow.",
    );
    lines.push(
        "- Paths: Relative paths in SKILL.md resolve relative to the skill directory containing SKILL.md.",
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
        assert!(catalog.contains("## Skills"));
        assert!(catalog.contains("$demo"));
        assert!(catalog.contains("(repo)"));
        assert!(catalog.contains("A demo skill"));
        assert!(catalog.contains("/tmp/demo/SKILL.md"));
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
        assert!(catalog.contains("(repo)"));
        assert!(catalog.contains("(user)"));
    }
}
