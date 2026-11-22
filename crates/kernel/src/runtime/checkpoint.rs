//! Workspace snapshots around each Run (M4b spec §5).
//!
//! The Kernel knows when a Run starts and ends but nothing of Git: an
//! injected [`RunCheckpoints`] takes the snapshots, and a top-level Session's
//! Run asks it for one before its first model request and one after its
//! work, before the Run reports its end.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use protocol::{EntryId, MessageId, RunId, SessionId};
use store::EntryKind;

use crate::runtime::Session;

/// How long a Run waits for its start snapshot before going on without one.
const START_BUDGET: Duration = Duration::from_secs(5);
/// How long a Run waits for its end snapshot.
const END_BUDGET: Duration = Duration::from_secs(10);

/// Which of a Run's two snapshots is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointStage {
    /// Before the Run's first model request.
    Start,
    /// After the Run's work, before it reports its end.
    End,
}

/// One snapshot request: the workspace, the Session and Run it belongs to,
/// and, for the end, the Run's last assistant message.
#[derive(Debug, typed_builder::TypedBuilder)]
pub struct CheckpointPoint<'a> {
    /// The Session's working directory.
    pub cwd: &'a Path,
    /// The Session the Run belongs to.
    pub session_id: &'a SessionId,
    /// The Run the snapshot belongs to.
    pub run_id: &'a RunId,
    /// Which snapshot this is.
    pub stage: CheckpointStage,
    /// The Run's last assistant message; only an end snapshot names one.
    #[builder(default)]
    pub anchor: Option<&'a MessageId>,
}

/// Takes and removes workspace snapshots for the Kernel.
#[async_trait::async_trait]
pub trait RunCheckpoints: Send + Sync {
    /// Snapshots the workspace; a workspace outside Git is skipped with `Ok`.
    async fn capture(&self, point: CheckpointPoint<'_>) -> Result<(), String>;

    /// Removes every snapshot of a Session.
    async fn remove(
        &self,
        cwd: &Path,
        session_id: &SessionId,
    ) -> Result<(), String>;
}

/// A taken start snapshot: where the Session's branch ended at that moment,
/// so the end only looks at what the Run itself added.
pub(in crate::runtime) struct RunStartMark {
    /// The branch leaf before the Run added anything; `None` at the root.
    leaf: Option<EntryId>,
}

/// The snapshots of one Run of one Session.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct RunCheckpointing<'a> {
    checkpoints: Option<&'a Arc<dyn RunCheckpoints>>,
    session: &'a Arc<Session>,
    session_id: &'a SessionId,
    run_id: &'a RunId,
}

impl RunCheckpointing<'_> {
    /// Takes the start snapshot within its budget. Returns its mark when it
    /// was taken: without it there is nothing to compare an end with.
    ///
    /// Subagent Sessions are skipped; their changes land within their
    /// owner's Run and count there.
    pub(in crate::runtime) async fn start(&self) -> Option<RunStartMark> {
        let checkpoints = self.checkpoints?;
        if self.session.ownership.is_some() {
            return None;
        }
        // Read before the Run inserts its question, so the end's search for
        // the Run's reply stops at what came before.
        let leaf = match self.session.transcript.leaf_id() {
            Ok(leaf) => leaf,
            Err(error) => {
                tracing::warn!(
                    "failed to read the branch of session {} before Run {}: {}",
                    self.session_id,
                    self.run_id,
                    error
                );
                return None;
            }
        };
        let point = self.point(CheckpointStage::Start, None);
        match tokio::time::timeout(START_BUDGET, checkpoints.capture(point))
            .await
        {
            Ok(Ok(())) => Some(RunStartMark { leaf }),
            Ok(Err(error)) => {
                tracing::warn!(
                    "failed start snapshot of Run {} in session {}: {}",
                    self.run_id,
                    self.session_id,
                    error
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    "abandoned start snapshot of Run {} in session {} after {:?}",
                    self.run_id,
                    self.session_id,
                    START_BUDGET
                );
                None
            }
        }
    }

    /// Takes the end snapshot within its budget, naming the Run's last reply.
    pub(in crate::runtime) async fn end(&self, start: &RunStartMark) {
        let Some(checkpoints) = self.checkpoints else {
            return;
        };
        let anchor = self.anchor(start);
        let point = self.point(CheckpointStage::End, anchor.as_ref());
        match tokio::time::timeout(END_BUDGET, checkpoints.capture(point)).await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                "failed end snapshot of Run {} in session {}: {}",
                self.run_id,
                self.session_id,
                error
            ),
            Err(_) => tracing::warn!(
                "abandoned end snapshot of Run {} in session {} after {:?}",
                self.run_id,
                self.session_id,
                END_BUDGET
            ),
        }
    }

    /// One snapshot request of this Run.
    fn point<'p>(
        &'p self,
        stage: CheckpointStage,
        anchor: Option<&'p MessageId>,
    ) -> CheckpointPoint<'p> {
        CheckpointPoint::builder()
            .cwd(self.session.cwd.as_path())
            .session_id(self.session_id)
            .run_id(self.run_id)
            .stage(stage)
            .anchor(anchor)
            .build()
    }

    /// The Run's last reply: walking the active branch from its leaf, the
    /// first assistant message met before any user message, among the
    /// entries the Run added after `start`. A Run that produced no reply, or
    /// was cancelled before asking, has none.
    fn anchor(&self, start: &RunStartMark) -> Option<MessageId> {
        let mut found = None;
        let mut done = false;
        let walked = self.session.transcript.inspect_active_branch(|entry| {
            // Entries from the start leaf on belong to earlier Runs.
            if start.leaf.as_ref() == Some(&entry.id) {
                done = true;
            }
            if done || entry.kind != EntryKind::Message {
                return;
            }
            let role = entry
                .payload
                .get("content")
                .and_then(|content| content.get("type"))
                .and_then(serde_json::Value::as_str);
            match role {
                Some("assistant") => {
                    found = entry
                        .payload
                        .get("message_id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|id| MessageId::try_from(id).ok());
                    done = true;
                }
                Some("user") => done = true,
                _ => {}
            }
        });
        if let Err(error) = walked {
            tracing::warn!(
                "failed to find the last reply of Run {} in session {}: {}",
                self.run_id,
                self.session_id,
                error
            );
        }
        found
    }
}
