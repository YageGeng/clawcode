use std::collections::BTreeSet;

use protocol::{
    AgentMessage, AssistantMetadata, CompactionPolicy, ContentBlock,
    MessageContent, ModelProfile, StopReason,
};

/// Provides kernel-specific overflow classification for shared compaction policy data.
pub(in crate::runtime) trait CompactionPolicyExt {
    /// Classifies pi-compatible overflow signals and whether they need one resumed Turn.
    fn overflow_recovery(
        &self,
        metadata: &AssistantMetadata,
        profile: &ModelProfile,
    ) -> Option<OverflowRecovery>;
}

impl CompactionPolicyExt for CompactionPolicy {
    /// Classifies pi-compatible overflow signals and whether they need one resumed Turn.
    fn overflow_recovery(
        &self,
        metadata: &AssistantMetadata,
        profile: &ModelProfile,
    ) -> Option<OverflowRecovery> {
        if !self.enabled
            || metadata.provider_id != profile.provider_id
            || metadata.model_id != profile.model_id
        {
            return None;
        }

        // Input already includes cached reads and writes in the upstream contract.
        let usage = metadata.usage.as_ref();
        if metadata.stop_reason == StopReason::EndTurn
            && usage
                .input_tokens
                .is_some_and(|input| input > profile.context_tokens)
        {
            return Some(OverflowRecovery::CompactOnly);
        }
        if metadata.stop_reason == StopReason::MaxTokens
            && (usage
                .output_tokens
                // Below the requested budget, a length stop means the window ran out.
                .is_some_and(|output| output < profile.request_max_tokens)
                || usage.input_tokens.is_some_and(|input| {
                    input.saturating_mul(100)
                        >= profile.context_tokens.saturating_mul(99)
                }))
        {
            return Some(OverflowRecovery::CompactAndRetry);
        }
        (metadata.stop_reason == StopReason::Error && metadata.context_overflow)
            .then_some(OverflowRecovery::CompactAndRetry)
    }
}

/// Bounded recovery action inferred from one completed Assistant attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum OverflowRecovery {
    /// Compact a silently over-window successful response without repeating it.
    CompactOnly,
    /// Remove the failed/truncated Assistant, compact, and start one new Turn.
    CompactAndRetry,
}

/// Provider-grounded estimate of the current active context size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ContextUsageEstimate {
    /// Estimated complete context tokens.
    pub tokens: u64,
    /// Tokens reported by the latest valid Assistant usage block.
    pub usage_tokens: u64,
    /// Estimated tokens after the usage-providing Assistant.
    pub trailing_tokens: u64,
    /// Message index that supplied usage, or `None` when no valid usage exists.
    #[builder(default, setter(strip_option))]
    pub last_usage_index: Option<usize>,
}

impl ContextUsageEstimate {
    /// Uses the latest successful Assistant usage plus estimated trailing messages.
    #[must_use]
    pub fn from_history(history: &[AgentMessage]) -> Self {
        let compaction_timestamp = history.iter().rev().find_map(|message| {
            matches!(message.content, MessageContent::CompactionSummary { .. })
                .then_some(message.timing.timestamp_ms)
        });
        let latest =
            history
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, message)| match &message.content {
                    MessageContent::Assistant { metadata, .. }
                        if !matches!(
                            metadata.stop_reason,
                            StopReason::Error | StopReason::Cancelled
                        ) && compaction_timestamp.is_none_or(
                            |boundary| message.timing.timestamp_ms > boundary,
                        ) =>
                    {
                        metadata
                            .usage
                            .as_ref()
                            .context_tokens()
                            .map(|tokens| (index, tokens))
                    }
                    MessageContent::System { .. }
                    | MessageContent::User { .. }
                    | MessageContent::ExpandedUser { .. }
                    | MessageContent::Assistant { .. }
                    | MessageContent::ToolResult { .. }
                    | MessageContent::BashExecution { .. }
                    | MessageContent::Extension { .. }
                    | MessageContent::SlashCommand { .. }
                    | MessageContent::CompactionSummary { .. }
                    | MessageContent::BranchSummary { .. } => None,
                });

        if let Some((index, usage_tokens)) = latest {
            let trailing_tokens = history
                .get(index.saturating_add(1)..)
                .unwrap_or_default()
                .iter()
                .fold(0_u64, |total, message| {
                    total.saturating_add(message.estimated_tokens())
                });
            return Self::builder()
                .tokens(usage_tokens.saturating_add(trailing_tokens))
                .usage_tokens(usage_tokens)
                .trailing_tokens(trailing_tokens)
                .last_usage_index(index)
                .build();
        }

        let estimated = history.iter().fold(0_u64, |total, message| {
            total.saturating_add(message.estimated_tokens())
        });
        Self::builder()
            .tokens(estimated)
            .usage_tokens(0)
            .trailing_tokens(estimated)
            .build()
    }

    /// Returns whether usage crossed the configured context reserve threshold.
    #[must_use]
    pub fn should_compact(
        &self,
        context_window: u64,
        policy: CompactionPolicy,
    ) -> bool {
        policy.enabled
            && self.tokens
                > context_window.saturating_sub(policy.reserve_tokens)
    }
}

/// Model input partition and deterministic pre-compaction token estimate.
pub(in crate::runtime) struct CompactionPreparation {
    pub(super) summarized: Vec<AgentMessage>,
    pub(super) turn_prefix: Vec<AgentMessage>,
    pub(super) retained_tail: Vec<AgentMessage>,
    pub(super) tokens_before: u64,
    pub(super) previous_summary: Option<String>,
    pub(super) file_operations: CompactionFileOperations,
    /// Budgets that selected this partition, reported to extensions.
    pub(super) policy: CompactionPolicy,
}

impl CompactionPreparation {
    /// Selects pi-compatible checkpoint, split-Turn prefix, and retained-tail ranges.
    pub(super) fn from_history(
        history: &[AgentMessage],
        policy: CompactionPolicy,
    ) -> Self {
        let previous_boundary = history.iter().rposition(|message| {
            matches!(message.content, MessageContent::CompactionSummary { .. })
        });
        let previous_summary = previous_boundary
            .and_then(|index| history.get(index))
            .and_then(|message| match &message.content {
                MessageContent::CompactionSummary { compaction } => {
                    Some(compaction.summary.clone())
                }
                _ => None,
            });
        let mut file_operations = previous_boundary
            .and_then(|index| history.get(index))
            .map(CompactionFileOperations::from)
            .unwrap_or_default();
        let compactable = previous_boundary
            .and_then(|index| history.get(index.saturating_add(1)..))
            .unwrap_or(history);
        let valid_cut_points = compactable
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                message.is_cut_point().then_some(index)
            })
            .collect::<Vec<_>>();
        // Walk newest to oldest like pi: once the recent budget is reached,
        // cut at the nearest cut point at or after that message. When only
        // trailing Tool results exceed the budget, keep their Assistant call
        // instead of falling back to the oldest cut point. A budget that is
        // never reached keeps everything from the first cut point.
        let mut first_kept = valid_cut_points.first().copied().unwrap_or(0);
        let mut retained_tokens = 0_u64;
        for (index, message) in compactable.iter().enumerate().rev() {
            let tokens = message.estimated_tokens();
            if tokens == 0 {
                continue;
            }
            retained_tokens = retained_tokens.saturating_add(tokens);
            if retained_tokens >= policy.keep_recent_tokens {
                first_kept = valid_cut_points
                    .iter()
                    .copied()
                    .find(|cut| *cut >= index)
                    .or_else(|| valid_cut_points.last().copied())
                    .unwrap_or(first_kept);
                break;
            }
        }
        let cut_starts_turn = compactable
            .get(first_kept)
            .is_some_and(CompactionBoundary::starts_turn);
        let turn_start = (!cut_starts_turn && first_kept < compactable.len())
            .then(|| {
                compactable
                    .get(..=first_kept)?
                    .iter()
                    .rposition(CompactionBoundary::starts_turn)
            })
            .flatten();
        let history_end = turn_start.unwrap_or(first_kept);
        let summarized = compactable.get(..history_end).unwrap_or_default();
        let turn_prefix = turn_start
            .and_then(|start| compactable.get(start..first_kept))
            .unwrap_or_default();
        // Direct Slash Commands and display-only extension/bash records remain
        // replayable in Store but are not part of pi's retained model context.
        let retained_tail = compactable
            .get(first_kept..)
            .unwrap_or_default()
            .iter()
            .filter(|message| message.estimated_tokens() > 0)
            .cloned()
            .collect::<Vec<_>>();
        file_operations.extend(summarized);
        file_operations.extend(turn_prefix);

        Self {
            summarized: summarized.to_vec(),
            turn_prefix: turn_prefix.to_vec(),
            retained_tail,
            tokens_before: ContextUsageEstimate::from_history(history).tokens,
            previous_summary,
            file_operations,
            policy,
        }
    }
}

impl From<&CompactionPreparation> for protocol::CompactionPreparation {
    /// Projects the partition into the extension event shape.
    fn from(preparation: &CompactionPreparation) -> Self {
        // ponytail: clones the partition once per compaction; borrow if
        // extension events become hot.
        protocol::CompactionPreparation::builder()
            .messages_to_summarize(preparation.summarized.clone())
            .turn_prefix_messages(preparation.turn_prefix.clone())
            .retained_tail(preparation.retained_tail.clone())
            .is_split_turn(!preparation.turn_prefix.is_empty())
            .tokens_before(preparation.tokens_before)
            .previous_summary(preparation.previous_summary.clone())
            .read_files(preparation.file_operations.read_files())
            .modified_files(preparation.file_operations.modified_files())
            .settings(preparation.policy)
            .build()
    }
}

/// Cumulative file access metadata retained across compaction boundaries.
#[derive(Default)]
pub(in crate::runtime) struct CompactionFileOperations {
    read: BTreeSet<String>,
    modified: BTreeSet<String>,
}

impl CompactionFileOperations {
    /// Collects canonical pi file-tool paths from messages removed from direct context.
    pub(in crate::runtime) fn extend(&mut self, messages: &[AgentMessage]) {
        for block in messages
            .iter()
            .filter_map(|message| {
                let MessageContent::Assistant { blocks, .. } = &message.content
                else {
                    return None;
                };
                Some(blocks)
            })
            .flatten()
        {
            let ContentBlock::ToolCall {
                name, arguments, ..
            } = block
            else {
                continue;
            };
            let Some(path) =
                arguments.get("path").and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            match name.as_str() {
                "read" => {
                    self.read.insert(path.to_string());
                }
                "write" | "edit" => {
                    self.modified.insert(path.to_string());
                }
                _ => {}
            }
        }
    }

    /// Returns sorted read paths for deterministic storage and replay.
    pub(in crate::runtime) fn read_files(&self) -> Vec<String> {
        self.read.difference(&self.modified).cloned().collect()
    }

    /// Returns sorted modified paths for deterministic storage and replay.
    pub(in crate::runtime) fn modified_files(&self) -> Vec<String> {
        self.modified.iter().cloned().collect()
    }

    /// Absorbs a nested branch summary's cumulative file lists.
    pub(in crate::runtime) fn extend_branch_summary_files(
        &mut self,
        branch: &protocol::BranchSummaryMessage,
    ) {
        self.read.extend(branch.read_files.iter().cloned());
        self.modified.extend(branch.modified_files.iter().cloned());
    }

    /// Renders pi's `<read-files>` and `<modified-files>` summary suffix.
    pub(in crate::runtime) fn summary_suffix(&self) -> String {
        let mut suffix = String::new();
        for (tag, files) in [
            ("read-files", self.read_files()),
            ("modified-files", self.modified_files()),
        ] {
            if !files.is_empty() {
                suffix.push_str(&format!(
                    "\n\n<{tag}>\n{}\n</{tag}>",
                    files.join("\n")
                ));
            }
        }
        suffix
    }
}

impl From<&AgentMessage> for CompactionFileOperations {
    /// Restores cumulative file metadata from the latest compaction boundary.
    fn from(message: &AgentMessage) -> Self {
        let MessageContent::CompactionSummary { compaction } = &message.content
        else {
            return Self::default();
        };
        Self {
            read: compaction.read_files.iter().cloned().collect(),
            modified: compaction.modified_files.iter().cloned().collect(),
        }
    }
}

/// Pi-compatible message boundaries used to partition history for compaction.
pub(in crate::runtime) trait CompactionBoundary {
    /// Reports whether this message opens a new Turn of model context.
    fn starts_turn(&self) -> bool;

    /// Reports whether compaction may keep history starting at this message.
    fn is_cut_point(&self) -> bool;
}

impl CompactionBoundary for AgentMessage {
    fn starts_turn(&self) -> bool {
        match &self.content {
            // A branch summary opens a Turn like pi's branchSummary message.
            MessageContent::User { .. }
            | MessageContent::ExpandedUser { .. }
            | MessageContent::BashExecution { .. }
            | MessageContent::BranchSummary { .. } => true,
            // Only model-visible extension messages act like user input; a
            // hidden diagnostic can sit between Tool results, and cutting
            // there would strand the next result from its retained call.
            MessageContent::Extension { extension } => {
                extension.include_in_context
            }
            MessageContent::System { .. }
            | MessageContent::Assistant { .. }
            | MessageContent::ToolResult { .. }
            | MessageContent::SlashCommand { .. }
            | MessageContent::CompactionSummary { .. } => false,
        }
    }

    fn is_cut_point(&self) -> bool {
        // Tool results must stay behind their call, so they are never cut points.
        self.starts_turn()
            || matches!(self.content, MessageContent::Assistant { .. })
    }
}

/// Conservative four-characters-per-token projection for trailing messages.
pub(in crate::runtime) trait EstimatedTokens {
    /// Estimates one complete message without provider tokenizer access.
    fn estimated_tokens(&self) -> u64;
}

impl EstimatedTokens for AgentMessage {
    fn estimated_tokens(&self) -> u64 {
        let blocks = match &self.content {
            MessageContent::System { blocks }
            | MessageContent::User { blocks }
            | MessageContent::Assistant { blocks, .. }
            | MessageContent::ToolResult { blocks, .. } => blocks,
            MessageContent::ExpandedUser { expansion } => {
                &expansion.model_blocks
            }
            MessageContent::BashExecution { bash } => {
                if bash.exclude_from_context {
                    return 0;
                }
                let characters = bash.model_text().chars().count();
                return u64::try_from(characters.div_ceil(4))
                    .unwrap_or(u64::MAX);
            }
            MessageContent::Extension { extension }
                if extension.include_in_context =>
            {
                let characters = serde_json::to_string(extension)
                    .map_or(0_usize, |value| value.chars().count());
                return u64::try_from(characters.div_ceil(4))
                    .unwrap_or(u64::MAX);
            }
            MessageContent::Extension { .. }
            | MessageContent::SlashCommand { .. } => return 0,
            MessageContent::BranchSummary { branch } => {
                let characters = branch.summary.chars().count();
                return u64::try_from(characters.div_ceil(4))
                    .unwrap_or(u64::MAX);
            }
            MessageContent::CompactionSummary { compaction } => {
                let characters = compaction.summary.chars().count();
                return u64::try_from(characters.div_ceil(4))
                    .unwrap_or(u64::MAX);
            }
        };
        let characters = blocks.iter().fold(0_usize, |total, block| {
            let block_characters = match block {
                ContentBlock::Text { text }
                | ContentBlock::Reasoning { text } => text.chars().count(),
                // Binary media uses Pi's fixed image estimate instead of
                // counting base64 bytes that are not sent as plain text.
                ContentBlock::Image { .. } | ContentBlock::Audio { .. } => {
                    4_800
                }
                ContentBlock::EmbeddedResource {
                    uri,
                    mime_type,
                    content,
                } => {
                    let metadata = uri.chars().count().saturating_add(
                        mime_type
                            .as_deref()
                            .map_or(0_usize, |value| value.chars().count()),
                    );
                    metadata.saturating_add(match content {
                        protocol::EmbeddedResourceContent::Text { text } => {
                            text.chars().count()
                        }
                        protocol::EmbeddedResourceContent::Blob { .. } => 4_800,
                    })
                }
                ContentBlock::ResourceLink {
                    uri,
                    name,
                    title,
                    description,
                    mime_type,
                    ..
                } => [
                    Some(uri.as_str()),
                    Some(name.as_str()),
                    title.as_deref(),
                    description.as_deref(),
                    mime_type.as_deref(),
                ]
                .into_iter()
                .flatten()
                .fold(0_usize, |count, value| {
                    count.saturating_add(value.chars().count())
                }),
                ContentBlock::Structured { value } => {
                    serde_json::to_string(value)
                        .map_or(0_usize, |value| value.chars().count())
                }
                ContentBlock::ToolCall {
                    name, arguments, ..
                } => name.chars().count().saturating_add(
                    serde_json::to_string(arguments)
                        .map_or(0_usize, |value| value.chars().count()),
                ),
            };
            total.saturating_add(block_characters)
        });
        u64::try_from(characters.div_ceil(4)).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A length stop that used the whole request budget is a normal truncation, not an overflow.
    #[test]
    fn length_stop_at_request_budget_is_not_overflow() {
        let profile = ModelProfile::builder()
            .provider_id("p".to_string())
            .model_id("m".to_string())
            .display_name("m".to_string())
            .context_tokens(100_000)
            .max_output_tokens(64_000)
            .request_max_tokens(16_000)
            .build();
        let policy = CompactionPolicy::builder()
            .enabled(true)
            .reserve_tokens(16_384)
            .keep_recent_tokens(20_000)
            .build();
        let metadata = |output: u64| {
            AssistantMetadata::builder()
                .provider_id("p".to_string())
                .model_id("m".to_string())
                .stop_reason(StopReason::MaxTokens)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new()
                        .input_tokens(1_000)
                        .output_tokens(output),
                ))
                .build()
        };

        assert_eq!(policy.overflow_recovery(&metadata(16_000), &profile), None);
        assert_eq!(
            policy.overflow_recovery(&metadata(9_000), &profile),
            Some(OverflowRecovery::CompactAndRetry)
        );
    }

    /// Wraps one content value in a fixed identity and timing for partition tests.
    fn message(content: MessageContent) -> AgentMessage {
        let timestamp = protocol::TimestampMs::from(1);
        AgentMessage {
            identity: protocol::MessageIdentity {
                message_id: protocol::MessageId::try_from("message")
                    .expect("message id"),
                turn_id: protocol::TurnId::try_from("turn").expect("turn id"),
            },
            timing: protocol::MessageTiming::try_from((
                timestamp, timestamp, timestamp,
            ))
            .expect("message timing"),
            content,
        }
    }

    /// Builds one text block of a chosen length, which drives the token estimate.
    fn text_blocks(length: usize) -> Vec<ContentBlock> {
        vec![ContentBlock::Text {
            text: "x".repeat(length),
        }]
    }

    /// Builds an Assistant message that only carries text.
    fn assistant(length: usize) -> AgentMessage {
        message(MessageContent::Assistant {
            blocks: text_blocks(length),
            metadata: AssistantMetadata::builder()
                .provider_id("p".to_string())
                .model_id("m".to_string())
                .stop_reason(StopReason::ToolUse)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new().input_tokens(0).output_tokens(0),
                ))
                .build(),
            model_message: None,
        })
    }

    /// Builds a user message.
    fn user() -> AgentMessage {
        message(MessageContent::User {
            blocks: text_blocks(8),
        })
    }

    /// Builds a ToolResult with a chosen output length.
    fn tool_result(length: usize) -> AgentMessage {
        message(MessageContent::ToolResult {
            tool_call_id: protocol::ToolCallId::try_from("call")
                .expect("tool call id"),
            blocks: text_blocks(length),
            is_error: false,
            details: None,
            output: None,
            cancelled: false,
        })
    }

    /// Builds an Extension message that is or is not part of model context.
    fn extension(length: usize, include_in_context: bool) -> AgentMessage {
        message(MessageContent::Extension {
            extension: protocol::ExtensionMessage {
                extension_id: protocol::ExtensionId::try_from("ext")
                    .expect("extension id"),
                custom_type: "note".to_string(),
                blocks: text_blocks(length),
                display: true,
                include_in_context,
                details: None,
            },
        })
    }

    /// Builds a policy that keeps roughly `keep_recent_tokens` of recent context.
    fn keep_recent(keep_recent_tokens: u64) -> CompactionPolicy {
        CompactionPolicy::builder()
            .enabled(true)
            .reserve_tokens(16_384)
            .keep_recent_tokens(keep_recent_tokens)
            .build()
    }

    /// A hidden diagnostic between tool results cannot strand a result from its call.
    #[test]
    fn hidden_extension_is_not_a_compaction_cut_point() {
        let history = [
            user(),
            assistant(16),
            extension(16, false),
            tool_result(200),
            extension(16, false),
            tool_result(200),
        ];

        let preparation =
            CompactionPreparation::from_history(&history, keep_recent(60));

        // No cut point follows the budget boundary, so the newest Assistant
        // call is kept with its results and its user prefix is summarized.
        assert!(matches!(
            preparation.retained_tail.first().map(|m| &m.content),
            Some(MessageContent::Assistant { .. })
        ));
        assert_eq!(preparation.turn_prefix.len(), 1);
        assert_eq!(
            preparation
                .retained_tail
                .iter()
                .filter(|m| matches!(
                    m.content,
                    MessageContent::ToolResult { .. }
                ))
                .count(),
            2
        );
    }

    /// A branch summary opens a Turn and is a place compaction may cut, like pi.
    #[test]
    fn branch_summary_starts_a_turn() {
        let summary = message(MessageContent::BranchSummary {
            branch: protocol::BranchSummaryMessage::builder()
                .entry_id(protocol::EntryId::try_from("entry").expect("entry"))
                .from_id("root".to_string())
                .summary("branch".to_string())
                .build(),
        });

        assert!(summary.starts_turn());
        assert!(summary.is_cut_point());
    }

    /// Trailing Tool results larger than the budget keep their Assistant call instead of everything.
    #[test]
    fn oversized_trailing_tool_results_fall_back_to_last_cut_point() {
        let history = [
            user(),
            assistant(40),
            user(),
            assistant(16),
            tool_result(400),
        ];

        let preparation =
            CompactionPreparation::from_history(&history, keep_recent(60));

        assert_eq!(preparation.summarized.len(), 2);
        assert_eq!(preparation.turn_prefix.len(), 1);
        assert!(matches!(
            preparation.retained_tail.first().map(|m| &m.content),
            Some(MessageContent::Assistant { .. })
        ));
        assert_eq!(preparation.retained_tail.len(), 2);
    }

    /// A zero budget still keeps the newest cut point, matching pi.
    #[test]
    fn zero_keep_recent_keeps_the_last_cut_point() {
        let history = [user(), assistant(40), user(), assistant(40)];

        let preparation =
            CompactionPreparation::from_history(&history, keep_recent(0));

        assert_eq!(preparation.summarized.len(), 2);
        assert_eq!(preparation.turn_prefix.len(), 1);
        assert_eq!(preparation.retained_tail.len(), 1);
    }

    /// A visible Extension message starts a Turn, so cutting there is not a split Turn.
    #[test]
    fn visible_extension_starts_a_turn() {
        let history =
            [user(), assistant(400), extension(400, true), assistant(40)];

        let preparation =
            CompactionPreparation::from_history(&history, keep_recent(60));

        assert!(preparation.turn_prefix.is_empty());
        assert_eq!(preparation.summarized.len(), 2);
    }

    /// An Extension message the model reads stays a place compaction may cut.
    #[test]
    fn visible_extension_stays_a_compaction_cut_point() {
        let history =
            [user(), assistant(400), extension(400, true), assistant(40)];

        let preparation =
            CompactionPreparation::from_history(&history, keep_recent(60));

        assert!(matches!(
            preparation.retained_tail.first().map(|m| &m.content),
            Some(MessageContent::Extension { .. })
        ));
    }
}
