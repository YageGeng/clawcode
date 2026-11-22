//! Per-run workspace snapshots a client reaches through
//! `_clawcode/session/checkpoints` and `_clawcode/session/checkpoint_diff`:
//! which runs changed the workspace, and each changed file's patch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{MessageId, RunId, SessionId, WorkspaceChanges};

/// Parameters naming a session's snapshots in its workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointsParameters {
    /// Absolute workspace directory: the session's working directory.
    pub root: PathBuf,
    /// Session whose runs are listed.
    pub session_id: SessionId,
}

/// One run that changed the workspace between its start and end snapshots.
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
pub struct RunCheckpoint {
    /// The run the snapshots bracket.
    pub run_id: RunId,
    /// The run's last assistant message, under which a client shows the
    /// change; `None` when the run produced no reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub anchor_message_id: Option<MessageId>,
    /// When the start snapshot was taken, as Unix milliseconds.
    pub started_at_ms: u64,
    /// The files the run changed and their line counts.
    pub changes: WorkspaceChanges,
}

/// A session's runs that changed the workspace, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointList {
    /// Runs in start order.
    pub runs: Vec<RunCheckpoint>,
}

/// Parameters of one file's change within one run.
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
pub struct CheckpointDiffParameters {
    /// Absolute workspace directory: the session's working directory.
    pub root: PathBuf,
    /// Session the run belongs to.
    pub session_id: SessionId,
    /// Run whose snapshots are compared.
    pub run_id: RunId,
    /// Root-relative path of the file.
    pub path: String,
    /// The file's path before a rename, so the diff shows the rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub original_path: Option<String>,
    /// The path's bytes in hex when its name is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub path_bytes: Option<String>,
}

impl CheckpointDiffParameters {
    /// The file's root-relative path as the file system knows it.
    pub fn file_path(&self) -> PathBuf {
        crate::workspace::file_path(&self.path, self.path_bytes.as_deref())
    }
}
