use protocol::{
    AgentMessage, ContentBlock, MessageContent, ModelUsage, StopReason,
};

use super::super::*;

/// Immutable inputs for one tool-free model summary request.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct SummaryGeneration<'a> {
    pub(in crate::runtime) session: &'a Arc<Session>,
    pub(in crate::runtime) run_id: &'a RunId,
    pub(in crate::runtime) turn_id: &'a TurnId,
    pub(in crate::runtime) timestamp: TimestampMs,
    pub(in crate::runtime) messages: Vec<AgentMessage>,
    pub(in crate::runtime) cancellation: &'a CancellationToken,
    pub(in crate::runtime) protocol: SummaryProtocol<'a>,
}

/// Selects the prompt contract without conflating compaction and branch summaries.
pub(in crate::runtime) enum SummaryProtocol<'a> {
    /// Pi-style checkpoint generation with optional incremental context.
    Compaction {
        previous_summary: Option<&'a str>,
        custom_instructions: Option<&'a str>,
    },
    /// Pi-style summary for the discarded prefix of one oversized Turn.
    TurnPrefix,
    /// Existing branch-summary behavior with caller-owned instructions.
    Branch { instruction: &'a str },
}

/// Complete successful summary response required by compaction persistence.
pub(in crate::runtime) struct GeneratedSummary {
    /// Non-empty summary text after trimming provider output.
    pub(in crate::runtime) text: String,
    /// Provider token accounting for this standalone request.
    pub(in crate::runtime) usage: ModelUsage,
    /// Provider-normalized terminal reason retained by usage records.
    pub(in crate::runtime) stop_reason: StopReason,
}

/// Type-driven renderer for the one-user-message summarization protocol.
pub(super) struct SummaryPrompt<'a> {
    pub(super) messages: &'a [AgentMessage],
    pub(super) previous_summary: Option<&'a str>,
    pub(super) custom_instructions: Option<&'a str>,
    pub(super) template: SummaryTemplate,
}

/// Chooses the exact pi checkpoint body appended after serialized conversation.
#[derive(Clone, Copy)]
pub(super) enum SummaryTemplate {
    Checkpoint,
    TurnPrefix,
}

impl SummaryPrompt<'_> {
    /// Serializes model-visible history and the fixed checkpoint contract.
    pub(super) fn render(&self) -> String {
        let mut conversation = Vec::new();
        for message in self.messages {
            match &message.content {
                MessageContent::User { .. }
                | MessageContent::ExpandedUser { .. } => {
                    if let Some(text) = message.content.text_content() {
                        conversation.push(format!("[User]: {text}"));
                    }
                }
                MessageContent::Assistant { blocks, .. } => {
                    let reasoning = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Reasoning { text } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !reasoning.is_empty() {
                        conversation
                            .push(format!("[Assistant thinking]: {reasoning}"));
                    }
                    let text = blocks
                        .iter()
                        .filter_map(ContentBlock::text)
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        conversation.push(format!("[Assistant]: {text}"));
                    }
                    let tool_calls = blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => Some(format!("{name}({arguments})")),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    if !tool_calls.is_empty() {
                        conversation.push(format!(
                            "[Assistant tool calls]: {tool_calls}"
                        ));
                    }
                }
                MessageContent::ToolResult { .. } => {
                    if let Some(text) = message.content.text_content() {
                        let truncated = if text.chars().count() > 2_000 {
                            let prefix =
                                text.chars().take(2_000).collect::<String>();
                            let omitted =
                                text.chars().count().saturating_sub(2_000);
                            format!(
                                "{prefix}\n\n[... {omitted} more characters truncated]"
                            )
                        } else {
                            text
                        };
                        conversation
                            .push(format!("[Tool result]: {truncated}"));
                    }
                }
                MessageContent::BashExecution { bash }
                    if !bash.exclude_from_context =>
                {
                    conversation.push(format!("[User]: {}", bash.model_text()));
                }
                MessageContent::Extension { extension }
                    if extension.include_in_context =>
                {
                    let text = extension
                        .blocks
                        .iter()
                        .filter_map(ContentBlock::text)
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        conversation.push(format!("[User]: {text}"));
                    }
                }
                MessageContent::System { .. }
                | MessageContent::BashExecution { .. }
                | MessageContent::Extension { .. }
                | MessageContent::SlashCommand { .. }
                | MessageContent::CompactionSummary { .. } => {}
            }
        }

        let mut prompt = format!(
            "<conversation>\n{}\n</conversation>\n\n",
            conversation.join("\n\n")
        );
        match self.template {
            SummaryTemplate::Checkpoint => {
                if let Some(previous_summary) = self.previous_summary {
                    prompt.push_str("<previous-summary>\n");
                    prompt.push_str(previous_summary);
                    prompt.push_str("\n</previous-summary>\n\n");
                    prompt.push_str(UPDATE_SUMMARIZATION_PROMPT);
                } else {
                    prompt.push_str(SUMMARIZATION_PROMPT);
                }
            }
            SummaryTemplate::TurnPrefix => {
                prompt.push_str(TURN_PREFIX_SUMMARIZATION_PROMPT)
            }
        }
        if let Some(custom_instructions) = self
            .custom_instructions
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty())
        {
            prompt.push_str("\n\nAdditional focus: ");
            prompt.push_str(custom_instructions);
        }
        prompt
    }
}

/// Dedicated model role contract used only for context summarization.
pub(super) const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// Exact checkpoint structure used by pi for the first summary.
const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// Exact checkpoint update structure used when a previous summary exists.
const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// Exact pi prompt used when recent retention cuts through one oversized Turn.
const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix."#;
