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
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
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
            .input_tokens(0)
            .output_tokens(0)
            .cache_read_tokens(0)
            .cache_write_tokens(0)
            .total_tokens(0)
            .build();
        for message in transcript {
            match &message.content {
                MessageContent::User { .. }
                | MessageContent::ExpandedUser { .. } => {
                    statistics.user_messages += 1;
                }
                MessageContent::Assistant { blocks, metadata } => {
                    statistics.assistant_messages += 1;
                    statistics.tool_calls += blocks
                        .iter()
                        .filter(|block| {
                            matches!(block, ContentBlock::ToolCall { .. })
                        })
                        .count();
                    statistics.input_tokens = statistics
                        .input_tokens
                        .saturating_add(metadata.usage.input_tokens);
                    statistics.output_tokens = statistics
                        .output_tokens
                        .saturating_add(metadata.usage.output_tokens);
                    statistics.cache_read_tokens = statistics
                        .cache_read_tokens
                        .saturating_add(metadata.usage.cache_read_tokens);
                    statistics.cache_write_tokens = statistics
                        .cache_write_tokens
                        .saturating_add(metadata.usage.cache_write_tokens);
                    statistics.total_tokens = statistics
                        .total_tokens
                        .saturating_add(metadata.usage.total_tokens);
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
        writeln!(formatter, "- Input tokens: {}", self.input_tokens)?;
        writeln!(formatter, "- Output tokens: {}", self.output_tokens)?;
        writeln!(formatter, "- Cache read tokens: {}", self.cache_read_tokens)?;
        writeln!(
            formatter,
            "- Cache write tokens: {}",
            self.cache_write_tokens
        )?;
        write!(formatter, "- Total tokens: {}", self.total_tokens)
    }
}
