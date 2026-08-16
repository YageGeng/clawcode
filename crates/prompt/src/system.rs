use std::collections::{BTreeMap, BTreeSet};

use protocol::{
    ProductIdentity, SkillInfo, SystemPromptBuildOptions, SystemPromptTool,
};

use crate::PromptSession;

/// Turn-specific tool, Skill, and policy inputs for System Prompt construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptTurnInput {
    /// Selected tool snapshot shared with the Provider request.
    pub tools: Vec<SystemPromptTool>,
    /// Effective Session Skill metadata.
    pub skills: Vec<SkillInfo>,
    /// Whether configuration permits Skill catalog instructions.
    pub include_skill_instructions: bool,
}

/// Rendered System Prompt paired with its complete structured inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSystemPrompt {
    /// Provider-facing System Prompt text.
    pub text: String,
    /// Structured values exposed to the before-agent-start Extension hook.
    pub options: SystemPromptBuildOptions,
}

impl PromptSession {
    /// Builds one dynamic System Prompt from the current Turn tool snapshot.
    #[must_use]
    pub fn build_system_prompt(
        &self,
        input: SystemPromptTurnInput,
    ) -> BuiltSystemPrompt {
        let selected_tools: Vec<_> =
            input.tools.iter().map(|tool| tool.name.clone()).collect();
        let tool_contributions: BTreeMap<_, _> = input
            .tools
            .iter()
            .map(|tool| (tool.name.clone(), tool.contribution.clone()))
            .collect();
        let options = SystemPromptBuildOptions::builder()
            .cwd(self.cwd().to_path_buf())
            .custom_prompt(self.custom_prompt().map(str::to_string))
            .append_system_prompt(
                self.append_system_prompt().map(str::to_string),
            )
            .selected_tools(selected_tools.clone())
            .tool_contributions(tool_contributions)
            .project_instructions(self.instructions().to_vec())
            .skills(input.skills.clone())
            .include_skill_instructions(input.include_skill_instructions)
            .build();
        let mut text = match self.custom_prompt() {
            Some(custom_prompt) => custom_prompt.to_string(),
            None => Self::default_system_prompt(&input.tools),
        };

        if let Some(append) = self
            .append_system_prompt()
            .filter(|append| !append.is_empty())
        {
            text.push_str("\n\n");
            text.push_str(append);
        }
        if !self.instructions().is_empty() {
            text.push_str(
                "\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n",
            );
            for instruction in self.instructions() {
                text.push_str("<project_instructions path=\"");
                text.push_str(&Self::escape_xml(
                    &instruction.path.to_string_lossy(),
                ));
                text.push_str("\">\n");
                text.push_str(&instruction.content);
                text.push_str("\n</project_instructions>\n\n");
            }
            text.push_str("</project_context>\n");
        }

        let has_read = selected_tools.iter().any(|name| name == "read");
        let has_model_invocable_skill = input
            .skills
            .iter()
            .any(|skill| !skill.disable_model_invocation);
        if has_read
            && input.include_skill_instructions
            && has_model_invocable_skill
        {
            text.push_str("\n\nThe following skills provide specialized instructions for specific tasks.\n");
            text.push_str("Use the read tool to load a skill's file when the task matches its description.\n");
            text.push_str("When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n\n");
            text.push_str("<available_skills>\n");
            for skill in input
                .skills
                .iter()
                .filter(|skill| !skill.disable_model_invocation)
            {
                text.push_str("  <skill>\n    <name>");
                text.push_str(&Self::escape_xml(&skill.name));
                text.push_str("</name>\n    <description>");
                text.push_str(&Self::escape_xml(&skill.description));
                text.push_str("</description>\n    <location>");
                text.push_str(&Self::escape_xml(&skill.path.to_string_lossy()));
                text.push_str("</location>\n  </skill>\n");
            }
            text.push_str("</available_skills>");
        }

        text.push_str("\nCurrent working directory: ");
        text.push_str(&self.cwd().to_string_lossy().replace('\\', "/"));

        BuiltSystemPrompt { text, options }
    }

    /// Renders the built-in identity, visible tools, and deduplicated guidelines.
    fn default_system_prompt(tools: &[SystemPromptTool]) -> String {
        let visible_tools: Vec<_> = tools
            .iter()
            .filter_map(|tool| {
                let snippet = tool
                    .contribution
                    .snippet
                    .as_deref()?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                (!snippet.is_empty())
                    .then(|| format!("- {}: {snippet}", tool.name))
            })
            .collect();
        let tools_list = if visible_tools.is_empty() {
            "(none)".to_string()
        } else {
            visible_tools.join("\n")
        };

        let mut guidelines = Vec::new();
        let mut seen = BTreeSet::new();
        let has_bash = tools.iter().any(|tool| tool.name == "bash");
        let has_specialized_file_tool = tools
            .iter()
            .any(|tool| matches!(tool.name.as_str(), "grep" | "find" | "ls"));
        if has_bash && !has_specialized_file_tool {
            let fallback =
                "Use bash for file operations like ls, rg, find".to_string();
            seen.insert(fallback.clone());
            guidelines.push(fallback);
        }
        for guideline in
            tools.iter().flat_map(|tool| &tool.contribution.guidelines)
        {
            let normalized = guideline.trim();
            if !normalized.is_empty() && seen.insert(normalized.to_string()) {
                guidelines.push(normalized.to_string());
            }
        }
        for fixed in [
            "Be concise in your responses",
            "Show file paths clearly when working with files",
        ] {
            if seen.insert(fixed.to_string()) {
                guidelines.push(fixed.to_string());
            }
        }
        let guidelines = guidelines
            .into_iter()
            .map(|guideline| format!("- {guideline}"))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "You are an expert coding assistant operating inside {}, an agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{tools_list}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.\n\nGuidelines:\n{guidelines}",
            ProductIdentity::NAME
        )
    }

    /// Escapes XML metadata while preserving instruction Markdown bodies verbatim.
    fn escape_xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}
