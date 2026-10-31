use std::fmt::{Display, Formatter};

use protocol::{AgentMessage, ContentBlock, MessageContent, SessionId};

/// Persisted Session statistics rendered by the `/session` Builtin.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct SessionStatistics {
    session_id: SessionId,
    #[builder(default)]
    name: Option<String>,
    user_messages: usize,
    assistant_messages: usize,
    tool_results: usize,
    tool_calls: usize,
    usage: protocol::ModelUsage,
}

impl SessionStatistics {
    /// Aggregates replayable message and provider usage facts without estimating cost.
    pub(super) fn from_transcript(
        session_id: SessionId,
        name: Option<String>,
        transcript: &[AgentMessage],
    ) -> Self {
        let mut statistics = Self::builder()
            .session_id(session_id)
            .name(name)
            .user_messages(0)
            .assistant_messages(0)
            .tool_results(0)
            .tool_calls(0)
            .usage(protocol::ModelUsage::default())
            .build();
        for message in transcript {
            match &message.content {
                MessageContent::User { .. }
                | MessageContent::ExpandedUser { .. } => {
                    statistics.user_messages += 1;
                }
                MessageContent::Assistant {
                    blocks, metadata, ..
                } => {
                    statistics.assistant_messages += 1;
                    statistics.tool_calls += blocks
                        .iter()
                        .filter(|block| {
                            matches!(
                                block,
                                ContentBlock::ToolCall {
                                    invalid_arguments: None,
                                    ..
                                }
                            )
                        })
                        .count();
                    // All summaries use the same upstream partial-counter semantics.
                    statistics.usage += &metadata.usage;
                }
                MessageContent::ToolResult { .. } => {
                    statistics.tool_results += 1;
                }
                MessageContent::System { .. }
                | MessageContent::BashExecution { .. }
                | MessageContent::Extension { .. }
                | MessageContent::SlashCommand { .. }
                | MessageContent::CompactionSummary { .. } => {}
            }
        }
        statistics
    }
}

impl Display for SessionStatistics {
    /// Renders stable Markdown without inventing unavailable monetary cost data.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(formatter, "## Session")?;
        writeln!(formatter, "- Session ID: `{}`", self.session_id)?;
        if let Some(name) = &self.name {
            writeln!(formatter, "- Name: {name}")?;
        }
        writeln!(formatter, "- User messages: {}", self.user_messages)?;
        writeln!(
            formatter,
            "- Assistant messages: {}",
            self.assistant_messages
        )?;
        writeln!(formatter, "- Tool calls: {}", self.tool_calls)?;
        writeln!(formatter, "- Tool results: {}", self.tool_results)?;
        let usage = self.usage.as_ref();
        // Keep CLI text in English and preserve the report's original lack of a trailing newline.
        for (index, (label, count)) in [
            ("Input tokens", usage.input_tokens),
            ("Output tokens", usage.output_tokens),
            ("Cache read tokens", usage.cached_input_tokens),
            ("Cache write tokens", usage.cache_creation_input_tokens),
            ("Total tokens", usage.total_tokens),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                writeln!(formatter)?;
            }
            match count {
                Some(count) => write!(formatter, "- {label}: {count}")?,
                None => write!(formatter, "- {label}: unknown")?,
            }
        }
        Ok(())
    }
}
