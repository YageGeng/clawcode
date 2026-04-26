use crate::model::SkillMetadata;

/// Renders the available skills section that is appended to the runtime system prompt.
pub fn render_skills_section(skills: &[SkillMetadata]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    lines.push("## Skills".to_string());
    lines.push("A skill is a local instruction file stored as `SKILL.md`. Use a skill when the user explicitly names it or the task clearly matches its description.".to_string());
    lines.push("### Available skills".to_string());

    for skill in skills {
        let path = skill.path.to_string_lossy().replace('\\', "/");
        lines.push(format!(
            "- {}: {} (file: {})",
            skill.name, skill.description, path
        ));
    }

    lines.push("### How to use skills".to_string());
    lines.push("When a skill applies, read the referenced `SKILL.md` content injected in the turn context and follow it for that turn only.".to_string());

    Some(lines.join("\n"))
}
