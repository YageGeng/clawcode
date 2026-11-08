use std::sync::Arc;

use protocol::{
    AgentMessage, ContentBlock, MessageContent, ModelUsage, RunId, StopReason,
    TimestampMs, TurnId,
};
use tokio_util::sync::CancellationToken;

use crate::runtime::Session;

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
    TurnPrefix {
        custom_instructions: Option<&'a str>,
    },
    /// Pi-style summary of an abandoned conversation branch.
    Branch {
        custom_instructions: Option<&'a str>,
        replace_instructions: bool,
    },
}

impl SummaryProtocol<'_> {
    /// Names the request in pi's failure text, such as a truncated summary.
    pub(in crate::runtime) fn failure_label(&self) -> &'static str {
        match self {
            Self::Compaction { .. } => "Summarization",
            Self::TurnPrefix { .. } => "Turn prefix summarization",
            Self::Branch { .. } => "Branch summarization",
        }
    }
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
    Branch { replace_instructions: bool },
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
                    // Reasoning is omitted like pi: providers return summaries
                    // of the real reasoning, replayed thinking is often not in
                    // the live context, and it can make the summary request
                    // larger than the context it summarizes.
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
                            } => Some(format!(
                                "{name}({})",
                                ToolArguments(arguments)
                            )),
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
                MessageContent::BranchSummary { branch } => {
                    conversation
                        .push(format!("[User]: {}", branch.model_text()));
                }
                // Only branch summaries can see an earlier checkpoint here:
                // compaction input starts after its previous boundary, whose
                // summary travels separately as the previous summary.
                MessageContent::CompactionSummary { compaction } => {
                    conversation
                        .push(format!("[User]: {}", compaction.model_text()));
                }
                MessageContent::System { .. }
                | MessageContent::BashExecution { .. }
                | MessageContent::Extension { .. }
                | MessageContent::SlashCommand { .. } => {}
            }
        }

        let conversation = conversation.join("\n\n");
        let custom_instructions = self
            .custom_instructions
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty());
        let mut prompt = match self.template {
            SummaryTemplate::Checkpoint | SummaryTemplate::Branch { .. } => {
                format!("<conversation>\n{conversation}\n</conversation>\n\n")
            }
            // Pi frames the prefix request with headings, not XML tags.
            SummaryTemplate::TurnPrefix => {
                format!("# Conversation\n{conversation}\n\n# Instructions\n")
            }
        };
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
            // Replacing instructions sends only the caller's text, like pi.
            SummaryTemplate::Branch {
                replace_instructions: true,
            } if custom_instructions.is_some() => {
                prompt.push_str(custom_instructions.unwrap_or_default());
                return prompt;
            }
            SummaryTemplate::Branch { .. } => {
                prompt.push_str(BRANCH_SUMMARY_PROMPT)
            }
        }
        if let Some(custom_instructions) = custom_instructions {
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

/// Exact pi preamble placed before a generated branch summary.
pub(in crate::runtime) const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

/// Exact pi structure for an abandoned-branch summary.
const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// Exact pi prompt used when recent retention cuts through one oversized Turn.
const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

/// Renders Tool-call arguments like pi: `key=<json>` pairs joined by `, `.
/// Argument order is not guaranteed.
struct ToolArguments<'a>(&'a serde_json::Value);

impl std::fmt::Display for ToolArguments<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some(object) = self.0.as_object() else {
            // Non-object arguments have no keys to pair.
            return write!(formatter, "{}", self.0);
        };
        for (index, (key, value)) in object.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{key}={value}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use protocol::{
        CompactionSummaryMessage, EntryId, MessageId, MessageIdentity,
        MessageTiming, TimestampMs, TurnId,
    };

    use super::*;

    /// A compaction checkpoint on an abandoned branch reaches the branch
    /// summary input as user context, like pi's `compactionSummary` message.
    #[test]
    fn branch_prompt_includes_compaction_checkpoints() {
        let timestamp = TimestampMs::from(1);
        let checkpoint = AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("checkpoint").expect("id"),
                turn_id: TurnId::try_from("turn").expect("turn"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("timing"),
            content: MessageContent::CompactionSummary {
                compaction: CompactionSummaryMessage::builder()
                    .entry_id(EntryId::try_from("entry").expect("entry"))
                    .summary("earlier work on the parser".to_string())
                    .tokens_before(42_000)
                    .build(),
            },
        };

        let prompt = SummaryPrompt {
            messages: std::slice::from_ref(&checkpoint),
            previous_summary: None,
            custom_instructions: None,
            template: SummaryTemplate::Branch {
                replace_instructions: false,
            },
        }
        .render();

        assert!(
            prompt.contains("[User]: The conversation history before this point was compacted from 42000 tokens"),
            "{prompt}"
        );
        assert!(prompt.contains("earlier work on the parser"), "{prompt}");
    }
}
