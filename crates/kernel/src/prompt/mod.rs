//! System prompt assembly pipeline.
//!
//! Constructs a layered system prompt string from agent-specific,
//! environment, instruction-file, skill, and user-provided sources.
//! The rendered result is injected into each LLM request via
//! [`CompletionRequest::preamble`].

pub(crate) mod environment;
pub(crate) mod instruction;

use environment::EnvironmentInfo;
pub(crate) use instruction::Instructions;

/// Default system prompt used when no agent-specific prompt is configured.
pub(crate) const DEFAULT_SYSTEM_PROMPT: &str = "\
You are an interactive coding agent. You help users with \
software engineering tasks: reading and editing code, running \
shell commands, searching codebases, and managing multi-agent workflows.

## Tool usage
- Prefer dedicated tools over bash: use Read (not cat), Edit/Write \
(not sed/awk), Grep (not rg), Glob (not find).
- Always read a file before editing it.
- When editing, use exact string matches — verify indentation and context.
- Do not create files the user did not ask for.

## Coding conventions
- Write concise, correct code. Do not add features or abstractions \
beyond the task.
- Three similar lines is better than a premature abstraction. \
No half-finished implementations.
- Default to no comments. Only add one when the WHY is non-obvious.

## Git safety
- Never run destructive git commands (push --force, reset --hard, \
checkout --, clean -f) unless the user explicitly requests them.
- Never skip hooks (--no-verify, --no-gpg-sign).
- Do not commit unless the user explicitly asks.

## Response style
- Respond concisely. Use Github-flavored markdown for formatting.
- When referencing code, include file_path:line_number.";

/// Layered system prompt whose [`render`](SystemPrompt::render) method
/// produces the final string injected as the LLM request preamble.
///
/// Assembly order: agent prompt, environment, instructions, skills, user prompt.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub(crate) struct SystemPrompt {
    /// Agent-specific prompt. When `Some`, completely replaces the
    /// default system prompt. When `None`, [`DEFAULT_SYSTEM_PROMPT`] is used.
    #[builder(default)]
    pub agent_prompt: Option<String>,
    /// Runtime environment snapshot.
    pub environment: EnvironmentInfo,
    /// Loaded instruction files (AGENTS.md + .agents/*.md).
    #[builder(default)]
    pub instructions: Option<Instructions>,
    /// Skill registry XML block. Only filled when the agent has skill
    /// permission and the registry is populated.
    #[builder(default)]
    pub skills_xml: Option<String>,
    /// Temporary user-provided system prompt. Lowest priority.
    #[builder(default)]
    pub user_prompt: Option<String>,
}

impl SystemPrompt {
    /// Render the complete system prompt string.
    ///
    /// Joins all non-empty layers in priority order with `\n`.
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // Agent prompt (or default)
        parts.push(
            self.agent_prompt
                .clone()
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
        );

        // Environment
        parts.push(self.environment.render_block());

        // Instructions
        if let Some(ref ins) = self.instructions {
            let block = ins.render();
            if !block.is_empty() {
                parts.push(block);
            }
        }

        // Skills
        if let Some(ref skills) = self.skills_xml {
            parts.push(skills.clone());
        }

        // User-provided prompt
        if let Some(ref user) = self.user_prompt {
            parts.push(user.clone());
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env() -> EnvironmentInfo {
        EnvironmentInfo::builder()
            .model_id("test-model".to_string())
            .cwd(std::path::PathBuf::from("/tmp/test"))
            .is_git_repo(true)
            .platform("linux".to_string())
            .date("2026-01-01".to_string())
            .build()
    }

    #[test]
    fn render_with_default_prompt_includes_environment() {
        let sp = SystemPrompt::builder().environment(test_env()).build();
        let result = sp.render();
        assert!(result.contains(DEFAULT_SYSTEM_PROMPT));
        assert!(result.contains("test-model"));
        assert!(result.contains("/tmp/test"));
    }

    #[test]
    fn agent_prompt_replaces_default() {
        let sp = SystemPrompt::builder()
            .environment(test_env())
            .agent_prompt(Some("Custom agent prompt".to_string()))
            .build();
        let result = sp.render();
        assert!(result.contains("Custom agent prompt"));
        assert!(!result.contains(DEFAULT_SYSTEM_PROMPT));
    }

    #[test]
    fn empty_layers_are_skipped() {
        let sp = SystemPrompt::builder().environment(test_env()).build();
        let result = sp.render();
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn user_prompt_appended_last() {
        let sp = SystemPrompt::builder()
            .environment(test_env())
            .user_prompt(Some("Extra instruction".to_string()))
            .build();
        let result = sp.render();
        let user_pos = result.find("Extra instruction").unwrap();
        let env_pos = result.find("test-model").unwrap();
        assert!(
            user_pos > env_pos,
            "user prompt must appear after environment"
        );
    }
}
