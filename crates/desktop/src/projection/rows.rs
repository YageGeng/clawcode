//! The transcript's row types: messages, tools, compactions, and errors,
//! with the keys that identify them across updates.

use std::fmt;

use agent_client_protocol::v2::schema as wire;
use typed_builder::TypedBuilder;

use super::{AssistantMeta, CommandMeta};

/// Why the context was compacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    /// The user asked for it.
    Manual,
    /// The context reached its threshold.
    Threshold,
    /// The provider rejected an overflowing context.
    Overflow,
}

/// A completed context compaction.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct CompactionRow {
    /// Session entry holding the summary.
    pub entry_id: String,
    /// Why it ran.
    pub reason: CompactionReason,
    /// The summary, as Markdown.
    pub summary: String,
    /// Context size before compacting, in tokens.
    #[builder(default)]
    pub tokens_before: Option<u64>,
    /// How long compacting took.
    #[builder(default)]
    pub elapsed_ms: Option<u64>,
}

/// An extension handler that failed during the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRow {
    /// Stable id: `{turnId}:{sequence}`, or the persisted message id.
    pub id: String,
    /// Which extension and hook failed.
    pub source: String,
    /// What went wrong.
    pub message: String,
}

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person using the app.
    User,
    /// The agent.
    Assistant,
}

/// How a message entered the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageOrigin {
    /// A normal prompt or reply.
    Prompt,
    /// A shell command the user ran directly.
    UserBash,
    /// The summary written when leaving a branch.
    BranchSummary,
}

impl From<Option<&str>> for MessageOrigin {
    /// Maps the product `origin` field; anything unknown is a normal prompt.
    fn from(origin: Option<&str>) -> Self {
        match origin {
            Some("user_bash") => MessageOrigin::UserBash,
            Some("branch_summary") => MessageOrigin::BranchSummary,
            _ => MessageOrigin::Prompt,
        }
    }
}

/// One image attached to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBlock {
    /// Base64 image bytes.
    pub data: String,
    /// Media type such as `image/png`.
    pub mime_type: String,
}

/// A user or assistant message, including the assistant's thinking.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct MessageRow {
    /// Message identifier.
    pub message_id: wire::MessageId,
    /// Author.
    pub role: Role,
    /// Model turn, when known.
    #[builder(default)]
    pub turn_id: Option<String>,
    /// Visible text.
    #[builder(default)]
    pub text: String,
    /// Thinking text shown above the answer.
    #[builder(default)]
    pub reasoning: String,
    /// Attached images.
    #[builder(default)]
    pub images: Vec<ImageBlock>,
    /// Whether chunks are still arriving.
    #[builder(default)]
    pub streaming: bool,
    /// Message time in Unix milliseconds.
    #[builder(default)]
    pub timestamp_ms: Option<u64>,
    /// Where the message came from.
    #[builder(default = MessageOrigin::Prompt)]
    pub origin: MessageOrigin,
    /// How long the message took, from first to last output.
    ///
    /// ponytail: the backend reports only whole-message timing, so thinking
    /// time is approximated by it; add a reasoning timing when one exists.
    #[builder(default)]
    pub duration_ms: Option<u64>,
    /// Slash-command identity, on command messages.
    #[builder(default)]
    pub command: Option<CommandMeta>,
    /// Model, usage, and error, on assistant messages.
    #[builder(default)]
    pub assistant: Option<AssistantMeta>,
}

/// Lifecycle of one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Waiting to start, or paused.
    Pending,
    /// Executing.
    Running,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Stopped by the user.
    Cancelled,
    /// Refused by a policy.
    Blocked,
}

/// One tool call and its output so far.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct ToolRow {
    /// Tool call identifier.
    pub tool_call_id: wire::ToolCallId,
    /// Model turn, when known.
    #[builder(default)]
    pub turn_id: Option<String>,
    /// Tool name.
    #[builder(default)]
    pub name: Option<String>,
    /// Human-readable title.
    pub title: String,
    /// Tool category used to pick an icon.
    #[builder(default)]
    pub kind: Option<wire::ToolKind>,
    /// Current status.
    pub status: ToolStatus,
    /// Arguments as sent to the tool.
    #[builder(default)]
    pub raw_input: Option<serde_json::Value>,
    /// Text output collected so far.
    #[builder(default)]
    pub output: String,
    /// First event time in Unix milliseconds.
    #[builder(default)]
    pub timestamp_ms: Option<u64>,
    /// Unified diff of an edit, from the tool's diff content.
    #[builder(default)]
    pub patch: Option<String>,
    /// Text shown under a terminal's output, such as where the full output was saved.
    #[builder(default)]
    pub footer: Option<String>,
}

/// The kind of work a tool does; picks the icon, verb, and target field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    /// Reads a file.
    Read,
    /// Changes a file.
    Edit,
    /// Deletes a file.
    Delete,
    /// Moves a file.
    Move,
    /// Searches files.
    Search,
    /// Runs a command.
    Execute,
    /// Fetches a URL.
    Fetch,
    /// Reasons without side effects.
    Think,
    /// Anything else.
    Other,
}

impl From<Option<&wire::ToolKind>> for ToolCategory {
    /// Maps the ACP tool kind; a missing or newer kind is `Other`.
    fn from(kind: Option<&wire::ToolKind>) -> Self {
        match kind {
            Some(wire::ToolKind::Read) => ToolCategory::Read,
            Some(wire::ToolKind::Edit) => ToolCategory::Edit,
            Some(wire::ToolKind::Delete) => ToolCategory::Delete,
            Some(wire::ToolKind::Move) => ToolCategory::Move,
            Some(wire::ToolKind::Search) => ToolCategory::Search,
            Some(wire::ToolKind::Execute) => ToolCategory::Execute,
            Some(wire::ToolKind::Fetch) => ToolCategory::Fetch,
            Some(wire::ToolKind::Think) => ToolCategory::Think,
            _ => ToolCategory::Other,
        }
    }
}

impl MessageRow {
    /// Whether the message has anything to show: text, reasoning, images, or live streaming.
    pub fn is_visible(&self) -> bool {
        !self.text.is_empty()
            || !self.reasoning.is_empty()
            || !self.images.is_empty()
            || self.streaming
            // A failed model call shows its error even without any text.
            || self
                .assistant
                .as_ref()
                .is_some_and(|assistant| assistant.error.is_some())
    }
}

impl ToolCategory {
    /// Every category in declaration order.
    pub const ALL: [ToolCategory; 9] = [
        ToolCategory::Read,
        ToolCategory::Edit,
        ToolCategory::Delete,
        ToolCategory::Move,
        ToolCategory::Search,
        ToolCategory::Execute,
        ToolCategory::Fetch,
        ToolCategory::Think,
        ToolCategory::Other,
    ];
}

impl ToolRow {
    /// The tool's category.
    pub fn category(&self) -> ToolCategory {
        ToolCategory::from(self.kind.as_ref())
    }

    /// The first string argument worth showing next to the tool name.
    ///
    /// Search tools prefer the pattern; the rest prefer a path, command, or URL.
    /// `None` means the UI should fall back to the title.
    pub fn target(&self) -> Option<&str> {
        let keys: &[&str] = if self.category() == ToolCategory::Search {
            &["pattern", "path"]
        } else {
            &[
                "path",
                "file_path",
                "cmd",
                "command",
                "url",
                "query",
                "pattern",
            ]
        };
        let input = self.raw_input.as_ref()?;
        keys.iter()
            .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
    }

    /// Added and removed line counts of the patch, without the file headers.
    pub fn diff_stats(&self) -> Option<(usize, usize)> {
        let patch = self.patch.as_deref()?;
        let count = |sign: &str, header: &str| {
            patch
                .lines()
                .filter(|line| {
                    line.starts_with(sign) && !line.starts_with(header)
                })
                .count()
        };
        Some((count("+", "+++"), count("-", "---")))
    }

    /// Number of output lines.
    pub fn line_count(&self) -> usize {
        self.output.lines().count()
    }
}

/// One row of the transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptRow {
    /// A message.
    Message(MessageRow),
    /// A tool call.
    Tool(ToolRow),
    /// A completed context compaction.
    Compaction(CompactionRow),
    /// A failed extension handler.
    Error(ErrorRow),
}

impl TranscriptRow {
    /// Stable identity of the row, independent of its position.
    pub fn key(&self) -> RowKey {
        match self {
            TranscriptRow::Message(row) => {
                RowKey::Message(row.message_id.clone())
            }
            TranscriptRow::Tool(row) => RowKey::Tool(row.tool_call_id.clone()),
            TranscriptRow::Compaction(row) => {
                RowKey::Compaction(row.entry_id.clone())
            }
            TranscriptRow::Error(row) => RowKey::Error(row.id.clone()),
        }
    }
}

/// Stable identity of a transcript row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// A message row.
    Message(wire::MessageId),
    /// A tool row.
    Tool(wire::ToolCallId),
    /// A compaction row, by its session entry.
    Compaction(String),
    /// An error row.
    Error(String),
}

impl fmt::Display for RowKey {
    /// Renders `message:{id}`, `tool:{id}`, `compaction:{id}`, or `error:{id}`, the element id used by views.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowKey::Message(id) => write!(formatter, "message:{id}"),
            RowKey::Tool(id) => write!(formatter, "tool:{id}"),
            RowKey::Compaction(id) => write!(formatter, "compaction:{id}"),
            RowKey::Error(id) => write!(formatter, "error:{id}"),
        }
    }
}
