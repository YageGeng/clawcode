use std::collections::BTreeSet;

use protocol::{
    AgentMessage, AssistantMetadata, CompactionPolicy, ContentBlock,
    MessageContent, ModelProfile, ModelUsage, StopReason,
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

        let input_tokens = metadata
            .usage
            .input_tokens
            .saturating_add(metadata.usage.cache_read_tokens);
        if metadata.stop_reason == StopReason::EndTurn
            && input_tokens > profile.context_tokens
        {
            return Some(OverflowRecovery::CompactOnly);
        }
        if metadata.stop_reason == StopReason::MaxTokens
            && (metadata.usage.output_tokens < profile.max_output_tokens
                || input_tokens.saturating_mul(100)
                    >= profile.context_tokens.saturating_mul(99))
        {
            return Some(OverflowRecovery::CompactAndRetry);
        }
        if metadata.stop_reason != StopReason::Error {
            return None;
        }

        let error = metadata.error.as_deref()?.to_ascii_lowercase();
        if [
            "throttling error:",
            "service unavailable:",
            "rate limit",
            "too many requests",
        ]
        .iter()
        .any(|pattern| error.contains(pattern))
        {
            return None;
        }
        [
            "prompt is too long",
            "request_too_large",
            "input is too long for requested model",
            "exceeds the context window",
            "maximum context length",
            "input token count",
            "maximum prompt length",
            "reduce the length of the messages",
            "maximum allowed input length",
            "exceeds the available context size",
            "greater than the context length",
            "context window exceeds limit",
            "exceeded model token limit",
            "too large for model with",
            "configured context size",
            "model_context_window_exceeded",
            "prompt too long",
            "range of input length should be",
            "context_length_exceeded",
            "context length exceeded",
            "too many tokens",
            "token limit exceeded",
            "400 status code (no body)",
            "413 status code (no body)",
        ]
        .iter()
        .any(|pattern| error.contains(pattern))
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated complete context tokens.
    pub tokens: u64,
    /// Tokens reported by the latest valid Assistant usage block.
    pub usage_tokens: u64,
    /// Estimated tokens after the usage-providing Assistant.
    pub trailing_tokens: u64,
    /// Message index that supplied usage, or `None` when no valid usage exists.
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
                        ) && metadata.usage.context_tokens() > 0
                            && compaction_timestamp.is_none_or(
                                |boundary| {
                                    message.timing.timestamp_ms > boundary
                                },
                            ) =>
                    {
                        Some((index, metadata.usage.context_tokens()))
                    }
                    MessageContent::System { .. }
                    | MessageContent::User { .. }
                    | MessageContent::ExpandedUser { .. }
                    | MessageContent::Assistant { .. }
                    | MessageContent::ToolResult { .. }
                    | MessageContent::BashExecution { .. }
                    | MessageContent::Extension { .. }
                    | MessageContent::SlashCommand { .. }
                    | MessageContent::CompactionSummary { .. } => None,
                });

        if let Some((index, usage_tokens)) = latest {
            let trailing_tokens = history
                .get(index.saturating_add(1)..)
                .unwrap_or_default()
                .iter()
                .fold(0_u64, |total, message| {
                    total.saturating_add(message.estimated_tokens())
                });
            return Self {
                tokens: usage_tokens.saturating_add(trailing_tokens),
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(index),
            };
        }

        let estimated = history.iter().fold(0_u64, |total, message| {
            total.saturating_add(message.estimated_tokens())
        });
        Self {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        }
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
                matches!(
                    message.content,
                    MessageContent::User { .. }
                        | MessageContent::ExpandedUser { .. }
                        | MessageContent::Assistant { .. }
                        | MessageContent::BashExecution { .. }
                        | MessageContent::Extension { .. }
                )
                .then_some(index)
            })
            .collect::<Vec<_>>();
        let first_kept = if policy.keep_recent_tokens == 0 {
            compactable.len()
        } else {
            let mut selected = valid_cut_points.first().copied().unwrap_or(0);
            let mut retained_tokens = 0_u64;
            for (index, message) in compactable.iter().enumerate().rev() {
                retained_tokens =
                    retained_tokens.saturating_add(message.estimated_tokens());
                if retained_tokens >= policy.keep_recent_tokens {
                    selected = valid_cut_points
                        .iter()
                        .copied()
                        .find(|cut| *cut >= index)
                        .unwrap_or(selected);
                    break;
                }
            }
            selected
        };
        let cut_starts_turn =
            compactable.get(first_kept).is_some_and(|message| {
                matches!(
                    message.content,
                    MessageContent::User { .. }
                        | MessageContent::ExpandedUser { .. }
                        | MessageContent::BashExecution { .. }
                )
            });
        let turn_start = (!cut_starts_turn && first_kept < compactable.len())
            .then(|| {
                compactable.get(..=first_kept)?.iter().rposition(|message| {
                    matches!(
                        message.content,
                        MessageContent::User { .. }
                            | MessageContent::ExpandedUser { .. }
                            | MessageContent::BashExecution { .. }
                    )
                })
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
        }
    }
}

/// Cumulative file access metadata retained across compaction boundaries.
#[derive(Default)]
pub(super) struct CompactionFileOperations {
    read: BTreeSet<String>,
    modified: BTreeSet<String>,
}

impl CompactionFileOperations {
    /// Collects canonical pi file-tool paths from messages removed from direct context.
    fn extend(&mut self, messages: &[AgentMessage]) {
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
    pub(super) fn read_files(&self) -> Vec<String> {
        self.read.difference(&self.modified).cloned().collect()
    }

    /// Returns sorted modified paths for deterministic storage and replay.
    pub(super) fn modified_files(&self) -> Vec<String> {
        self.modified.iter().cloned().collect()
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

/// Token-accounting projection for provider usage.
trait ContextTokens {
    /// Returns provider total tokens or a checked component fallback.
    fn context_tokens(&self) -> u64;
}

impl ContextTokens for ModelUsage {
    fn context_tokens(&self) -> u64 {
        if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.input_tokens
                .saturating_add(self.output_tokens)
                .saturating_add(self.cache_read_tokens)
                .saturating_add(self.cache_write_tokens)
        }
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
