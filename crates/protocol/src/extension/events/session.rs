use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, CompactionPolicy, CompactionReason, CompactionResult,
    EntryId, SessionId, SessionTreeEntry,
};

/// Reason a session runtime was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartReason {
    /// Application startup restored the selected session.
    Startup,
    /// A new empty session was created.
    New,
    /// A persisted session was resumed.
    Resume,
    /// A new session was forked from an existing branch.
    Fork,
}

/// A session runtime has started after resource discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStartEvent {
    /// Why this runtime was created.
    pub reason: SessionStartReason,
    /// Previously active session when this operation replaced one.
    pub previous_session_id: Option<SessionId>,
}

/// Persisted session display metadata changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfoChangedEvent {
    /// Current normalized name, or absent when cleared.
    pub name: Option<String>,
}

/// Reason the active session is about to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSwitchReason {
    /// A new session will replace the current session.
    New,
    /// A persisted session will replace the current session.
    Resume,
}

/// A session replacement is about to occur and may be cancelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBeforeSwitchEvent {
    /// Kind of replacement operation.
    pub reason: SessionSwitchReason,
    /// Destination session for resume operations.
    pub target_session_id: Option<SessionId>,
}

/// Position used when forking around a selected entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionForkPosition {
    /// Fork from the selected entry's parent.
    Before,
    /// Fork with the selected entry as the new leaf.
    At,
}

/// A session fork is about to occur and may be cancelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBeforeForkEvent {
    /// Entry selected by the caller.
    pub entry_id: EntryId,
    /// Whether the selected entry is included in the forked branch.
    pub position: SessionForkPosition,
}

/// Pi-compatible partition of model context prepared for one compaction.
///
/// Fields use snake_case like the enclosing `SessionBeforeCompactEvent`.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct CompactionPreparation {
    /// Messages that will be summarized and leave direct context.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Prefix of the Turn the cut splits, summarized separately.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Newest messages kept verbatim; an extension-built compaction echoes them.
    pub retained_tail: Vec<AgentMessage>,
    /// Whether the cut falls inside a Turn.
    pub is_split_turn: bool,
    /// Effective context size before compaction.
    pub tokens_before: u64,
    /// Summary of the previous compaction, updated incrementally.
    #[builder(default)]
    pub previous_summary: Option<String>,
    /// Files only read, cumulative across checkpoints.
    pub read_files: Vec<String>,
    /// Files created or changed, cumulative across checkpoints.
    pub modified_files: Vec<String>,
    /// Compaction budgets in effect for the summarizing model.
    pub settings: CompactionPolicy,
}

/// Context compaction is about to occur and may be cancelled or replaced.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SessionBeforeCompactEvent {
    /// Partition the default summary would use.
    pub preparation: CompactionPreparation,
    /// Trigger for this compaction attempt.
    pub reason: CompactionReason,
    /// Whether overflow recovery retries the interrupted turn afterward.
    pub will_retry: bool,
    /// Active branch entries available to custom summary generation.
    pub branch_entries: Vec<SessionTreeEntry>,
    /// Optional caller instructions appended to summary generation.
    #[builder(default)]
    pub custom_instructions: Option<String>,
}

/// Context compaction completed and was persisted.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SessionCompactEvent {
    /// Persisted compaction result.
    pub compaction: CompactionResult,
    /// Whether an extension supplied the result.
    pub from_extension: bool,
    /// Trigger for this compaction.
    pub reason: CompactionReason,
    /// Whether overflow recovery retries the interrupted turn afterward.
    pub will_retry: bool,
}

/// Context compaction ended without persisting a checkpoint.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct SessionCompactFailedEvent {
    /// Trigger for the failed compaction.
    pub reason: CompactionReason,
    /// Failure text when compaction failed for a reason other than an abort.
    #[builder(default)]
    pub error_message: Option<String>,
    /// Whether the compaction was cancelled or aborted.
    #[builder(default)]
    pub aborted: bool,
    /// Whether the interrupted Turn would have been retried afterward.
    #[builder(default)]
    pub will_retry: bool,
    /// Whether the failing compaction content came from an extension.
    #[builder(default)]
    pub from_extension: bool,
}

/// Session-runtime shutdown reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionShutdownReason {
    /// Application or protocol shutdown.
    Quit,
    /// A new session replaces this runtime.
    New,
    /// A persisted session replaces this runtime.
    Resume,
    /// A forked session replaces this runtime.
    Fork,
}

/// A session runtime is shutting down before invalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionShutdownEvent {
    /// Why this runtime is ending.
    pub reason: SessionShutdownReason,
    /// Destination session for replacement operations.
    pub target_session_id: Option<SessionId>,
}

/// Session tree navigation is about to occur and may be cancelled or summarized.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SessionBeforeTreeEvent {
    /// Destination entry selected by the caller.
    pub target_id: EntryId,
    /// Current leaf before navigation.
    #[builder(default)]
    pub old_leaf_id: Option<EntryId>,
    /// Common ancestor of old and new branches.
    #[builder(default)]
    pub common_ancestor_id: Option<EntryId>,
    /// Entries that require summarization when requested.
    pub entries_to_summarize: Vec<SessionTreeEntry>,
    /// Whether the caller requested a branch summary.
    pub user_wants_summary: bool,
    /// Optional caller summary instructions.
    #[builder(default)]
    pub custom_instructions: Option<String>,
    /// Whether caller instructions replace host defaults.
    #[builder(default)]
    pub replace_instructions: bool,
    /// Optional summary label.
    #[builder(default)]
    pub label: Option<String>,
}

/// Session tree navigation completed.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
pub struct SessionTreeEvent {
    /// Leaf selected after navigation.
    #[builder(default)]
    pub new_leaf_id: Option<EntryId>,
    /// Leaf selected before navigation.
    #[builder(default)]
    pub old_leaf_id: Option<EntryId>,
    /// Persisted summary entry, when one was created.
    #[builder(default)]
    pub summary_entry: Option<SessionTreeEntry>,
    /// Whether an extension supplied the branch summary.
    pub from_extension: bool,
}
