use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use protocol::{
    EntryId, LaneId, RecordId, RunId, Sequence, SessionId, TimestampMs,
};
use serde::{Deserialize, Serialize};

/// Supplies timestamps to persistence without coupling tests to wall-clock time.
pub trait Clock: Send + Sync {
    /// Returns current Unix time in milliseconds.
    fn now(&self) -> TimestampMs;
}

/// Wall-clock implementation used by production store factories.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    /// Returns current Unix time in milliseconds, saturating to zero before the epoch.
    fn now(&self) -> TimestampMs {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_u128, |duration| duration.as_millis());
        TimestampMs::from(u64::try_from(millis).unwrap_or(u64::MAX))
    }
}

/// Options required to create one persisted session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCreateOptions {
    /// Stable session identifier.
    pub session_id: SessionId,

    /// Working directory encoded into the session directory name.
    pub cwd: PathBuf,

    /// Source session when this session was forked.
    pub parent_session_id: Option<SessionId>,
}

/// Options for copying one selected branch into a new session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionForkOptions {
    /// New-session identity, cwd, and optional parent override.
    pub create: SessionCreateOptions,

    /// Source branch leaf included in the fork.
    pub source_leaf: EntryId,

    /// Initial lane name created in the forked session.
    pub lane: LaneId,
}

/// Header and filesystem metadata returned without opening a session writer.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct SessionMetadata {
    /// Stable session identifier.
    pub id: SessionId,

    /// Session creation time from the v4 header.
    pub created_at_ms: TimestampMs,

    /// Working directory recorded by the session.
    pub cwd: PathBuf,

    /// Source session when this session was forked.
    #[builder(default)]
    pub parent_session_id: Option<SessionId>,

    /// Backing JSONL path.
    pub path: PathBuf,

    /// Filesystem modification time in Unix milliseconds.
    pub modified_at_ms: TimestampMs,

    /// Latest global session name fact, when one remains set.
    #[builder(default)]
    pub name: Option<String>,
}

/// Pi v4 entry discriminants persisted in session JSONL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Agent transcript message.
    Message,
    /// Provider or model selection change.
    ModelChange,
    /// Thinking-level selection change.
    ThinkingLevelChange,
    /// Active tool set change.
    ActiveToolsChange,
    /// Context compaction summary.
    Compaction,
    /// Navigation branch summary.
    BranchSummary,
    /// Application or extension-owned entry.
    Custom,
}

impl EntryKind {
    /// Returns the stable pi v4 entry discriminator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::ModelChange => "model_change",
            Self::ThinkingLevelChange => "thinking_level_change",
            Self::ActiveToolsChange => "active_tools_change",
            Self::Compaction => "compaction",
            Self::BranchSummary => "branch_summary",
            Self::Custom => "custom",
        }
    }
}

/// Entry fields supplied by a caller before storage assigns topology and time.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEntry {
    /// Stable entry identifier.
    pub id: EntryId,

    /// Persisted pi v4 entry kind.
    pub kind: EntryKind,

    /// Kind-specific fields flattened into the JSONL mutation.
    pub payload: serde_json::Value,
}

/// Pi v4 lane-record discriminants used for recovery and durable operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// Starts a run, compaction, or navigation operation.
    OperationStarted,
    /// Requests cancellation of an active run.
    AbortRequested,
    /// Completes, aborts, declines, or fails an operation.
    OperationFinished,
    /// Records one assistant, compaction, or branch-summary attempt.
    StepAttempt,
    /// Records a started tool invocation.
    ToolStarted,
    /// Adds a steering, follow-up, or next-run item.
    QueueEnqueued,
    /// Cancels one queued entry.
    QueueCancelled,
    /// Defers a write until recovery resumes the run.
    WriteDeferred,
    /// Records provider or adjustment token usage.
    Usage,
}

/// Lane-record fields supplied before storage assigns sequence and timestamp.
#[derive(Debug, Clone, PartialEq, typed_builder::TypedBuilder)]
pub struct NewRecord {
    /// Stable record identifier.
    pub id: RecordId,

    /// Lane that owns the operation record.
    pub lane: LaneId,

    /// Owning run when the record belongs to an active operation.
    #[builder(default)]
    pub run_id: Option<RunId>,

    /// Pi v4 lane-record discriminant.
    pub kind: RecordKind,

    /// Kind-specific fields flattened into the JSONL mutation.
    pub payload: serde_json::Value,
}

/// Complete stored entry after sequence, parent, and timestamp assignment.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SessionEntry {
    /// Stable entry identifier.
    pub id: EntryId,

    /// Shared session-log sequence.
    #[serde(rename = "seq")]
    pub sequence: Sequence,

    /// Previous entry on the appending lane.
    #[serde(rename = "parentId")]
    #[builder(default)]
    pub parent_id: Option<EntryId>,

    /// Storage-assigned Unix millisecond timestamp.
    #[serde(rename = "timestamp")]
    pub timestamp_ms: TimestampMs,

    /// Pi v4 entry discriminant.
    #[serde(rename = "type")]
    pub kind: EntryKind,

    /// Kind-specific fields preserved without schema loss.
    #[serde(flatten)]
    pub payload: serde_json::Map<String, serde_json::Value>,
}

/// Complete persisted lane record.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SessionRecord {
    /// Stable record identifier.
    pub id: RecordId,

    /// Shared session-log sequence.
    #[serde(rename = "seq")]
    pub sequence: Sequence,

    /// Lane that owns the record.
    pub lane: LaneId,

    /// Storage-assigned Unix millisecond timestamp.
    #[serde(rename = "timestamp")]
    pub timestamp_ms: TimestampMs,

    /// Owning run when present on the pi record type.
    #[serde(rename = "runId", skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub run_id: Option<RunId>,

    /// Pi v4 record discriminant.
    #[serde(rename = "type")]
    pub kind: RecordKind,

    /// Kind-specific fields preserved without schema loss.
    #[serde(flatten)]
    pub payload: serde_json::Map<String, serde_json::Value>,
}

/// Persistence errors surfaced by store factories and session logs.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem operation failed.
    #[error("store I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// JSONL value could not be encoded or decoded.
    #[error("store JSON failed: {0}")]
    Json(#[from] serde_json::Error),

    /// Caller referenced a missing or duplicate lane.
    #[error("invalid lane: {0}")]
    InvalidLane(String),

    /// Caller attempted to reuse an entry identifier.
    #[error("entry already exists: {0}")]
    DuplicateEntry(EntryId),

    /// Entry payload must be a JSON object for flattening into pi v4.
    #[error("entry payload must be a JSON object")]
    InvalidEntryPayload,

    /// Session log exceeded the supported sequence range.
    #[error("session sequence exceeded the supported range")]
    SequenceExhausted,

    /// Session JSONL schema or ordering is invalid.
    #[error("invalid session file: {0}")]
    InvalidSession(String),

    /// A fact referenced an entry that does not exist.
    #[error("entry not found: {0}")]
    EntryNotFound(EntryId),
}

/// Factory interface used by kernel construction to acquire session stores.
pub trait StoreFactory: Send + Sync {
    /// Creates a new persisted session and its initial v4 header.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError>;

    /// Opens an existing v4 JSONL session and repairs a torn final append.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError>;

    /// Forks one ancestor path into a new v4 session file.
    fn fork(
        &self,
        source_path: &Path,
        options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError>;

    /// Lists v4 sessions by scanning headers, optionally limited to one cwd.
    fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionMetadata>, StoreError>;

    /// Removes every persisted log owned by the stable session identifier.
    fn delete(&self, session_id: &SessionId) -> Result<(), StoreError>;
}

/// Mutable pi v4 session-log interface consumed by the kernel.
pub trait SessionStore: Send {
    /// Returns the stable session identifier from the v4 header.
    fn session_id(&self) -> &SessionId;

    /// Returns the backing JSONL file path.
    fn path(&self) -> &Path;

    /// Creates a new lane at an existing entry or at the root.
    fn create_lane(
        &mut self,
        lane: LaneId,
        at: Option<EntryId>,
    ) -> Result<(), StoreError>;

    /// Moves an existing lane to an entry or to the root without deleting history.
    fn move_lane(
        &mut self,
        lane: &LaneId,
        to: Option<EntryId>,
    ) -> Result<(), StoreError>;

    /// Appends an entry and advances the selected lane.
    fn append_entry(
        &mut self,
        lane: &LaneId,
        entry: NewEntry,
    ) -> Result<SessionEntry, StoreError>;

    /// Returns the current leaf of a lane.
    fn lane(&self, lane: &LaneId) -> Option<&EntryId>;

    /// Returns one stored entry by id.
    fn get_entry(&self, id: &EntryId) -> Option<&SessionEntry>;

    /// Returns all entries in shared sequence order.
    fn entries(&self) -> Vec<SessionEntry>;

    /// Returns one root-to-leaf branch while detecting broken or cyclic links.
    fn branch(&self, leaf: &EntryId) -> Result<Vec<SessionEntry>, StoreError>;

    /// Appends a durable operation record without moving a lane leaf.
    fn append_record(
        &mut self,
        record: NewRecord,
    ) -> Result<SessionRecord, StoreError>;

    /// Returns all durable lane records in shared sequence order.
    fn records(&self) -> Vec<SessionRecord>;

    /// Sets or clears the global session name fact.
    fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError>;

    /// Returns the latest global session name fact.
    fn name(&self) -> Option<&str>;

    /// Sets or clears a label on an existing entry.
    fn set_label(
        &mut self,
        target_id: EntryId,
        label: Option<String>,
    ) -> Result<(), StoreError>;

    /// Returns the latest global label fact for an entry.
    fn label(&self, target_id: &EntryId) -> Option<&str>;
}
