//! Trajectory protocol: the request snapshot each agent-loop attempt records,
//! the request contents addressed by hash, and the typed records and pages
//! the trajectory view reads.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    EntryId, ModelUsage, PromptCacheRetention, RunId, SessionId, StopReason,
    ThinkingLevel, TimestampMs, ToolCallId, TurnId,
};

/// Length of a lowercase hexadecimal SHA-256 digest.
const HASH_LEN: usize = 64;

/// SHA-256 of a request content, as 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);

/// A string that is not 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("content hash must be {HASH_LEN} lowercase hexadecimal digits")]
pub struct ContentHashError;

impl ContentHash {
    /// Hashes `bytes` with SHA-256.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};

        Self(
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// Returns the hexadecimal digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContentHash {
    type Error = ContentHashError;

    /// Accepts exactly 64 lowercase hexadecimal digits.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value.len() == HASH_LEN
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        valid.then_some(Self(value)).ok_or(ContentHashError)
    }
}

impl From<ContentHash> for String {
    /// Returns the wire form of the digest.
    fn from(value: ContentHash) -> Self {
        value.0
    }
}

impl fmt::Display for ContentHash {
    /// Writes the hexadecimal digest.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a request content holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestContentKind {
    /// The system prompt text.
    SystemPrompt,
    /// The JSON array of tool definitions.
    Tools,
}

impl RequestContentKind {
    /// Returns the wire name, as serde writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemPrompt => "system_prompt",
            Self::Tools => "tools",
        }
    }
}

/// A stored name that no variant of `kind` carries.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind}: {value}")]
pub struct UnknownVariant {
    /// The enum the name was read for.
    pub kind: &'static str,
    /// The name read.
    pub value: String,
}

impl std::str::FromStr for RequestContentKind {
    type Err = UnknownVariant;

    /// Reads the wire name [`Self::as_str`] writes.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::SystemPrompt, Self::Tools]
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or_else(|| UnknownVariant {
                kind: "request content kind",
                value: value.to_owned(),
            })
    }
}

/// One request content a Session's log stores once per hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContent {
    /// SHA-256 of `content`.
    pub hash: ContentHash,
    /// What `content` holds.
    pub kind: RequestContentKind,
    /// The system prompt text, or the tool definitions as JSON.
    pub content: String,
}

/// Generation options a request was sent with; the prompt-cache key is left out.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    /// Configuration identifier of the provider.
    pub provider_id: String,
    /// Provider-facing model identifier.
    pub model_id: String,
    /// Reasoning level of the request.
    pub thinking_level: ThinkingLevel,
    /// Requested output-token limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub max_tokens: Option<u64>,
    /// Requested sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub temperature: Option<f64>,
    /// Requested prompt-cache retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub prompt_cache: Option<PromptCacheRetention>,
}

/// What one agent-loop attempt sent to the provider, by content hash.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct RequestSnapshot {
    /// Hash of the system prompt text.
    pub system_prompt: ContentHash,
    /// Hash of the tool definitions JSON.
    pub tools: ContentHash,
    /// Generation options.
    pub options: RequestOptions,
    /// History messages sent, the system prompt not counted.
    pub message_count: u32,
}

/// A snapshot together with the two contents its hashes address.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestCapture {
    /// The snapshot the assistant attempt records.
    pub snapshot: RequestSnapshot,
    /// The system prompt, then the tools.
    pub contents: [RequestContent; 2],
}

/// Where a trajectory page starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryCursor {
    /// The newest page of the branch.
    Tail,
    /// The page that ends just before this sequence.
    Before(u64),
    /// The records after this sequence.
    After(u64),
}

/// A trajectory query naming both `before` and `after`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("before and after are mutually exclusive")]
pub struct TrajectoryCursorError;

/// Parameters of `_clawcode/session/trajectory`.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrajectoryParameters {
    /// Session to read.
    pub session_id: SessionId,
    /// Read the page before this sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub before: Option<u64>,
    /// Read the records after this sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub after: Option<u64>,
    /// Branch entries per page; the server applies its default and maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub limit: Option<u32>,
}

impl TrajectoryParameters {
    /// The cursor these parameters name; both `before` and `after` is an error.
    pub fn cursor(&self) -> Result<TrajectoryCursor, TrajectoryCursorError> {
        match (self.before, self.after) {
            (Some(_), Some(_)) => Err(TrajectoryCursorError),
            (Some(sequence), None) => Ok(TrajectoryCursor::Before(sequence)),
            (None, Some(sequence)) => Ok(TrajectoryCursor::After(sequence)),
            (None, None) => Ok(TrajectoryCursor::Tail),
        }
    }
}

/// Parameters of `_clawcode/session/trajectory/content`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrajectoryContentParameters {
    /// Session whose log holds the content.
    pub session_id: SessionId,
    /// Hash of the content.
    pub hash: ContentHash,
}

/// The Run a record belongs to and its position on the branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRef {
    /// Run identifier.
    pub id: RunId,
    /// One-based order of the Run on the branch.
    pub index: u32,
}

/// One row of a Session's trajectory.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryRecord {
    /// Stable id: the entry id, or a prefixed id such as `request:{entry}`.
    pub id: String,
    /// Sequence of the entry the record comes from.
    pub sequence: u64,
    /// Run the record belongs to; `None` between Runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub run: Option<RunRef>,
    /// Turn (one model request) the record belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub turn_id: Option<TurnId>,
    /// When the record started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub started_at_ms: Option<TimestampMs>,
    /// When the record ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub ended_at_ms: Option<TimestampMs>,
    /// What the record is.
    pub kind: TrajectoryRecordKind,
}

/// The kinds of trajectory records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TrajectoryRecordKind {
    /// A user message.
    User(UserRecord),
    /// A slash command or user shell command.
    Command(CommandRecord),
    /// The start of one model request.
    Request(RequestRecord),
    /// The output of one model request.
    Assistant(AssistantRecord),
    /// A tool call and its result.
    Tool(ToolRecord),
    /// A compaction checkpoint.
    Compaction {
        /// Summary text.
        summary: String,
        /// Context tokens before compaction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_before: Option<u64>,
    },
    /// A branch summary.
    BranchSummary {
        /// Summary text.
        summary: String,
    },
    /// A model, thinking-level, or active-tool change.
    Setting(SettingChange),
    /// An extension message.
    Extension(ExtensionRecord),
}

/// How a user message entered the Session.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum UserOrigin {
    /// Typed while the Session was idle.
    #[default]
    Input,
    /// Injected into a running Run.
    Steering,
    /// Queued for after the Run.
    FollowUp,
    /// Expanded from a skill or prompt template.
    Skill,
}

/// A user message.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    /// Text blocks joined.
    pub text: String,
    /// Image blocks counted, never carried.
    #[builder(default)]
    pub image_count: u32,
    /// Embedded resources and resource links counted.
    #[builder(default)]
    pub file_count: u32,
    /// How the message entered the Session.
    #[builder(default)]
    pub origin: UserOrigin,
}

/// Which command a command record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// A slash command.
    Slash,
    /// A user shell command.
    Bash,
}

/// A slash command or user shell command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRecord {
    /// Which command.
    pub kind: CommandKind,
    /// The command as typed.
    pub text: String,
}

/// The start of one model request and how it differs from the previous one.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    /// One-based request number on the branch.
    pub index: u32,
    /// What the request sent.
    pub snapshot: RequestSnapshot,
    /// The first request on the branch, compared with nothing.
    #[builder(default)]
    pub first: bool,
    /// The system prompt differs from the previous request's.
    #[builder(default)]
    pub prompt_changed: bool,
    /// Tool names added since the previous request, sorted.
    #[builder(default)]
    pub tools_added: Vec<String>,
    /// Tool names removed since the previous request, sorted.
    #[builder(default)]
    pub tools_removed: Vec<String>,
}

/// The output of one model request.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRecord {
    /// Number of the request that produced it.
    pub request_index: u32,
    /// Provider configuration identifier.
    pub provider_id: String,
    /// Provider-facing model identifier.
    pub model_id: String,
    /// Reasoning text.
    #[builder(default)]
    pub reasoning: String,
    /// Answer text.
    #[builder(default)]
    pub text: String,
    /// Tool calls the output requested, in order.
    #[builder(default)]
    pub tool_call_ids: Vec<ToolCallId>,
    /// Token usage of the request.
    pub usage: ModelUsage,
    /// When the request was sent.
    pub requested_at_ms: TimestampMs,
    /// When the first token arrived.
    pub first_token_at_ms: TimestampMs,
    /// When the last token arrived.
    pub last_token_at_ms: TimestampMs,
    /// Why generation stopped.
    pub stop_reason: StopReason,
    /// Terminal error of a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub error: Option<String>,
    /// The attempt was cancelled.
    #[builder(default)]
    pub cancelled: bool,
}

/// A tool call and, once it arrived, its result.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct ToolRecord {
    /// Call identifier.
    pub call_id: ToolCallId,
    /// Tool name.
    pub name: String,
    /// Call arguments.
    pub arguments: serde_json::Value,
    /// Result text; `None` while no result exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub result_text: Option<String>,
    /// Image blocks in the result, counted.
    #[builder(default)]
    pub image_count: u32,
    /// The tool failed.
    #[builder(default)]
    pub is_error: bool,
    /// Cancellation stopped the call.
    #[builder(default)]
    pub cancelled: bool,
}

/// A Session setting change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "change",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SettingChange {
    /// The model changed.
    Model {
        /// Provider configuration identifier.
        provider: String,
        /// Provider-facing model identifier.
        model: String,
    },
    /// The thinking level changed.
    ThinkingLevel {
        /// New level.
        level: ThinkingLevel,
    },
    /// The active tools changed.
    ActiveTools {
        /// Active tool names.
        tools: Vec<String>,
    },
}

/// An extension message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionRecord {
    /// Extension that wrote it.
    pub extension_id: String,
    /// Extension-defined message type.
    pub custom_type: String,
    /// Text blocks joined.
    pub text: String,
}

/// One page of a Session's active-branch trajectory.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryPage {
    /// Records in sequence order.
    pub records: Vec<TrajectoryRecord>,
    /// Current branch leaf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub leaf_id: Option<EntryId>,
    /// Older branch entries exist before this page.
    #[builder(default)]
    pub has_more_before: bool,
    /// After queries only: newer records exist beyond the limit.
    #[builder(default)]
    pub has_more_after: bool,
    /// Sequence of the first branch entry in the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub first_sequence: Option<u64>,
    /// Sequence of the last branch entry in the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub last_sequence: Option<u64>,
    /// Usage of every request on the branch before this page; left empty
    /// for `after` pages on the same branch, which only append.
    #[builder(default)]
    pub usage_before: ModelUsage,
    /// The `after` cursor is not on the current branch, which navigation
    /// replaced; this page is the new branch's tail and replaces everything
    /// loaded. Appends move the leaf without changing the branch.
    #[serde(default)]
    #[builder(default)]
    pub branch_changed: bool,
}
