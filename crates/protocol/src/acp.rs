use std::path::PathBuf;

use serde::Deserialize;

use crate::{EntryId, QueueId, SessionId, UserBashRequest};

/// Parameters shared by ACP extensions that reference one session.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpSessionParameters {
    /// Stable session identifier supplied by the client.
    pub session_id: SessionId,
}

/// Model-backed compaction uses the common session-only request shape.
pub type AcpCompactParameters = AcpSessionParameters;

/// Server-side bash uses the shared Kernel request without a duplicate ACP shape.
pub type AcpUserBashParameters = UserBashRequest;

/// Parameters for moving or branching the active session cursor.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpNavigateParameters {
    /// Session whose active lane moves.
    pub session_id: SessionId,
    /// Target entry, or the root when absent.
    pub entry_id: Option<EntryId>,
}

/// Errors produced while validating a client-supplied working directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcpWorkingDirectoryError {
    /// The path was not absolute.
    #[error("working directory must be absolute")]
    Relative,
    /// The path does not exist.
    #[error("working directory does not exist")]
    Missing,
    /// The path exists but is not a directory.
    #[error("working directory must be a directory")]
    NotDirectory,
}

/// Validated absolute existing directory accepted by session operations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "PathBuf")]
pub struct AcpWorkingDirectory(PathBuf);

impl AcpWorkingDirectory {
    /// Consumes the validated directory into its filesystem path.
    #[must_use]
    pub fn into_inner(self) -> PathBuf {
        self.0
    }
}

impl TryFrom<PathBuf> for AcpWorkingDirectory {
    type Error = AcpWorkingDirectoryError;

    /// Rejects relative, missing, and non-directory paths at the protocol boundary.
    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        if !path.is_absolute() {
            return Err(AcpWorkingDirectoryError::Relative);
        }
        if !path.exists() {
            return Err(AcpWorkingDirectoryError::Missing);
        }
        if !path.is_dir() {
            return Err(AcpWorkingDirectoryError::NotDirectory);
        }
        Ok(Self(path))
    }
}

/// Parameters for forking one selected branch into a new session.
#[derive(Debug, Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpForkParameters {
    /// Source session.
    pub session_id: SessionId,
    /// Source branch leaf included in the fork.
    pub entry_id: EntryId,
    /// Working directory assigned to the new session.
    pub cwd: AcpWorkingDirectory,
    /// Caller-selected identity, or a generated identity when absent.
    #[builder(default)]
    pub new_session_id: Option<SessionId>,
}

/// Parameters for removing one queued message by its stable identifier.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpPendingMessageRemoveParameters {
    /// Session that owns the queue.
    pub session_id: SessionId,
    /// Pending message to cancel and remove.
    pub queue_id: QueueId,
}

/// Parameters for replacing one persisted session title.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpSessionRenameParameters {
    /// Session being renamed.
    pub session_id: SessionId,
    /// Unvalidated client-supplied title.
    pub title: String,
}

/// Parameters for explicitly loading one discovered skill.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpSkillParameters {
    /// Session whose immutable Skill catalog is queried.
    pub session_id: SessionId,
    /// Discovered skill name.
    pub name: String,
}

/// Parameters for one registered non-UI extension command.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpCommandParameters {
    /// Session that owns command execution.
    pub session_id: SessionId,
    /// Registered command name.
    pub name: String,
    /// Opaque command arguments preserved without schema loss.
    #[serde(default)]
    pub arguments: serde_json::Value,
}
