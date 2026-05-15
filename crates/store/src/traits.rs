use std::io;
use std::path::Path;

use async_trait::async_trait;
use protocol::{SessionId, SessionInfo};

use super::record::CreateSessionParams;
use super::recorder::SessionRecorder;
use super::replay::ReplayedSession;

/// Session lifecycle persistence abstraction.
///
/// Implementations own the storage backend (filesystem, database, etc.)
/// and produce [`SessionRecorder`] handles for per-session appends.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Create a new session file, write `SessionMeta`, and append a manifest entry.
    /// Returns a recorder for subsequent turn/message appends, or `None` when persistence is disabled.
    async fn create_session(
        &self,
        params: CreateSessionParams,
    ) -> io::Result<Option<Box<dyn SessionRecorder>>>;

    /// Load a persisted session by id, returning replayed state and a recorder for appending.
    /// Returns `None` when the session is not found or has been archived.
    fn load_session(
        &self,
        session_id: &SessionId,
    ) -> io::Result<Option<(ReplayedSession, Box<dyn SessionRecorder>)>>;

    /// Mark a session closed in the manifest and flush its recorder.
    async fn close_session(
        &self,
        session_id: &SessionId,
        recorder: Option<&dyn SessionRecorder>,
    ) -> io::Result<()>;

    /// List persisted sessions from the manifest, optionally filtered by cwd.
    fn list_sessions(&self, cwd: Option<&Path>) -> io::Result<Vec<SessionInfo>>;
}
