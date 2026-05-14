//! Skill tool — loads a skill's full instructions and bundled resources
//! into the conversation context via a structured `<skill_content>` XML block.
//!
//! Format follows the OpenCode reference implementation.

use std::sync::Arc;

use async_trait::async_trait;
use skills::SkillRegistry;

use crate::{Tool, ToolContext};

/// Base description text, matching OpenCode's `skill.txt` verbatim.
const BASE_DESCRIPTION: &str = r#"Load a specialized skill when the task at hand matches one of the skills listed in the system prompt.

Use this tool to inject the skill's instructions and resources into current conversation. The output may contain detailed workflow guidance as well as references to scripts, files, etc in the same directory as the skill.

The skill name must match one of the skills listed in your system prompt."#;

/// Tool that loads a skill's full `SKILL.md` body and presents it alongside
/// a sampled file listing of the skill directory.
///
/// Holds an [`Arc<SkillRegistry>`] for skill lookups during execution.
pub struct SkillTool {
    registry: Arc<SkillRegistry>,
    /// Augmented description: base text + Markdown skill list.
    /// Built once at construction time from `registry.render_markdown_catalog()`.
    description: String,
}

impl SkillTool {
    /// Create a new skill tool backed by the given registry.
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        let description = Self::render_description(&registry);
        Self {
            registry,
            description,
        }
    }

    /// Build the full tool description: base text + Markdown skill list.
    fn render_description(registry: &SkillRegistry) -> String {
        let markdown = registry.render_markdown_catalog();
        format!("{BASE_DESCRIPTION}\n\n{markdown}")
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the skill from <available_skills> in the system prompt"
                }
            },
            "required": ["name"]
        })
    }

    fn needs_approval(&self, _arguments: &serde_json::Value, _ctx: &ToolContext) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<String, String> {
        let name = arguments["name"]
            .as_str()
            .ok_or("missing 'name' argument")?;

        // Look up the skill by name (case-insensitive).
        let skill = self.registry.lookup(name).ok_or_else(|| {
            let available: Vec<&str> = self
                .registry
                .enabled_skills()
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            format!(
                "Skill \"{name}\" not found. Available skills: {}",
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            )
        })?;

        // Read SKILL.md and strip YAML frontmatter so only the body is shown.
        let content = std::fs::read_to_string(&skill.path)
            .map_err(|e| format!("failed to read skill file: {e}"))?;
        let body = strip_frontmatter(&content);

        // Compute the base directory as a file:// URL.
        let dir = skill.path.parent().unwrap_or(std::path::Path::new("."));
        let base = format!("file://{}", dir.display());

        // List files in the skill directory (skip SKILL.md, limit 10).
        let files = list_skill_files(dir);

        // Build the <skill_content> output, matching OpenCode's format.
        let output = format!(
            "<skill_content name=\"{name}\">\n\
             # Skill: {name}\n\n\
             {body}\n\n\
             Base directory for this skill: {base}\n\
             Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\n\
             Note: file list is sampled.\n\n\
             <skill_files>\n\
             {files}\
             </skill_files>\n\
             </skill_content>",
            name = skill.name,
        );

        Ok(output)
    }
}

/// Strip YAML frontmatter from SKILL.md content.
///
/// Frontmatter is delimited by `---` on its own line at the start and end.
/// Returns the body content after the closing `---`, or the full content if
/// no frontmatter is detected.
fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return trimmed;
    }
    // Find the closing `---` delimiter.
    let after_first = &trimmed[3..];
    // The closing delimiter must be at the start of a line.
    if let Some(pos) = after_first.find("\n---") {
        let body_start = pos + 5; // skip "\n---\n" or "\n---\r\n"
        let body = &after_first[body_start..];
        return body.trim_start();
    }
    trimmed
}

/// List files in the skill directory (max 10), skipping SKILL.md itself.
/// Each file is formatted as `<file>/absolute/path</file>`.
fn list_skill_files(dir: &std::path::Path) -> String {
    let mut result = String::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return result,
    };

    let mut count = 0u32;
    for entry in entries.flatten() {
        if count >= 10 {
            break;
        }
        let path = entry.path();
        // Skip directories and SKILL.md itself.
        if path.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
        {
            continue;
        }
        result.push_str(&format!("<file>{}</file>\n", path.display()));
        count += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_frontmatter_with_valid_delimiters() {
        let input = "---\nname: demo\ndescription: test\n---\n\n# Body\n\nContent here\n";
        let result = strip_frontmatter(input);
        assert!(result.contains("# Body"));
        assert!(result.contains("Content here"));
        assert!(!result.contains("---"));
    }

    #[test]
    fn strip_frontmatter_no_frontmatter() {
        let input = "# Just a markdown file\n\nNo frontmatter here.\n";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn strip_frontmatter_only_opening_delimiter() {
        let input = "---\nunclosed frontmatter\n";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn list_skill_files_skips_skill_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SKILL.md"), "---\nname: test\n---\nbody\n").unwrap();
        std::fs::write(dir.path().join("script.sh"), "echo hi").unwrap();
        std::fs::write(dir.path().join("reference.md"), "# ref").unwrap();

        let result = list_skill_files(dir.path());
        assert!(!result.contains("SKILL.md"));
        assert!(result.contains("script.sh"));
        assert!(result.contains("reference.md"));
    }

    #[test]
    fn list_skill_files_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..15 {
            std::fs::write(dir.path().join(format!("file_{i}.txt")), "").unwrap();
        }
        let result = list_skill_files(dir.path());
        let count = result.lines().count();
        assert!(count <= 10);
    }
}
