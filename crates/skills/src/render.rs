use crate::model::SkillMetadata;

/// Renders the available skills section that is appended to the runtime system prompt.
pub fn render_skills_section(skills: &[SkillMetadata]) -> Option<String> {
    let visible_skills = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect::<Vec<_>>();
    if visible_skills.is_empty() {
        return None;
    }

    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description."
            .to_string(),
        "When a skill file references a relative path, resolve it against the skill directory before reading additional files."
            .to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible_skills {
        let path = skill.path.to_string_lossy().replace('\\', "/");
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", xml_escape(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            xml_escape(&skill.description)
        ));
        lines.push(format!("    <location>{}</location>", xml_escape(&path)));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());

    Some(lines.join("\n"))
}

/// Escapes XML-sensitive characters for prompt rendering.
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
