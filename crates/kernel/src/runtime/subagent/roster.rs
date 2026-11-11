//! The owner's in-memory roster of named children, derived from child headers.

use std::collections::BTreeMap;
use std::sync::Mutex;

use protocol::{
    AgentMail, AgentName, AgentOutcome, AgentPath, MailKind, SessionId, TurnId,
};
use serde::{Deserialize, Serialize};

use super::mail::{MailDelivery, cut_bytes};
use crate::runtime::session::{ClawcodeEntry, StatedNote};
use crate::runtime::{Kernel, KernelError, Session};

/// Codex agent status subset that survives between a child's Runs; its serde
/// form is Codex `agent_status_output_schema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime) enum ChildStatus {
    /// Spawned; its first Run has not settled.
    PendingInit,
    /// Last Run completed with its truncated final answer, if known.
    Completed(Option<String>),
    /// Last Run failed with its error message.
    Errored(String),
    /// Last Run was cancelled before answering.
    Interrupted,
}

impl ChildStatus {
    /// Reports the status an agent tool shows: a live Run reads as `running`.
    pub(in crate::runtime) fn report(
        &self,
        running: bool,
    ) -> Result<serde_json::Value, serde_json::Error> {
        if running {
            return Ok(serde_json::json!("running"));
        }
        serde_json::to_value(self)
    }
}

/// One child known to its owner.
#[derive(Debug, Clone)]
pub(in crate::runtime) struct ChildEntry {
    pub(in crate::runtime) session_id: SessionId,
    pub(in crate::runtime) nickname: String,
    pub(in crate::runtime) status: ChildStatus,
}

/// An owner's named children and the lock that admits new ones.
#[derive(Default)]
pub(in crate::runtime) struct AgentRoster {
    /// Serializes admission so names and running slots are checked atomically.
    pub(in crate::runtime) spawning: tokio::sync::Mutex<()>,
    /// Cached children; `None` until first loaded from child headers.
    children: Mutex<Option<BTreeMap<AgentName, ChildEntry>>>,
}

impl AgentRoster {
    /// Records a child whose first Run never started, so `PendingInit` cannot
    /// hold a running slot forever.
    pub(in crate::runtime) fn mark_start_failed(
        &self,
        name: &AgentName,
        message: String,
    ) -> Result<(), KernelError> {
        if let Some(roster) = &mut *self.children.lock()?
            && let Some(entry) = roster.get_mut(name)
            && entry.status == ChildStatus::PendingInit
        {
            entry.status = ChildStatus::Errored(message);
        }
        Ok(())
    }

    /// Drops a deleted child from the cached roster; an unloaded roster
    /// rereads headers, which no longer list it.
    pub(in crate::runtime) fn forget_child(
        &self,
        name: &AgentName,
    ) -> Result<(), KernelError> {
        if let Some(roster) = &mut *self.children.lock()? {
            roster.remove(name);
        }
        Ok(())
    }
}

impl Session {
    /// Loads this owner's children from headers on first use, then runs `f`
    /// on the cached roster; header scans are synchronous, so the lock never
    /// crosses an `await`.
    pub(in crate::runtime) fn with_children<R>(
        &self,
        kernel: &Kernel,
        f: impl FnOnce(&mut BTreeMap<AgentName, ChildEntry>) -> R,
    ) -> Result<R, KernelError> {
        let mut children = self.roster.children.lock()?;
        let roster = match &mut *children {
            Some(roster) => roster,
            None => {
                let owner_id = self.transcript.session_id()?;
                // ponytail: one synchronous scan of the cwd's headers per
                // owner and process; move it off the async worker if a
                // project ever holds thousands of Sessions.
                let loaded = kernel
                    .store_factory
                    .headers(Some(&self.cwd))?
                    .into_iter()
                    .filter_map(|header| {
                        let edge = header.ownership?;
                        (edge.session_id == owner_id).then_some((
                            edge.name,
                            ChildEntry {
                                session_id: header.id,
                                nickname: edge.nickname,
                                status: ChildStatus::Completed(None),
                            },
                        ))
                    })
                    .collect::<BTreeMap<_, _>>();
                // Runs carry the Session in their span, so the line omits it.
                tracing::info!("loaded {} children from headers", loaded.len());
                children.insert(loaded)
            }
        };
        Ok(f(roster))
    }

    /// States the owner's children before a model request when they differ
    /// from what its context last stated; children have no roster.
    pub(in crate::runtime) fn refresh_subagent_roster(
        &self,
        kernel: &Kernel,
        turn_id: &TurnId,
    ) -> Result<(), KernelError> {
        if self.ownership.is_some() {
            return Ok(());
        }
        let current = self.with_children(kernel, |children| {
            children.keys().map(AgentPath::child).collect()
        })?;
        self.refresh_note::<SubagentRosterNote>(kernel, turn_id, current)
    }
}

/// Hint appended when a child's answer is cut for its owner's context.
const ANSWER_TRUNCATED: &str =
    "\n\n[answer truncated; open the agent session for the full answer]";

impl Kernel {
    /// Records a child's settled Run in its owner's roster and mails the
    /// answer to `/root` as a queue-only `result`; an interrupted Run is
    /// recorded but not reported.
    pub(in crate::runtime) fn notify_owner(
        &self,
        child: &Session,
        outcome: &AgentOutcome,
        answer: &str,
    ) {
        let Some(edge) = &child.ownership else {
            return;
        };
        let owner = match self.session(&edge.session_id) {
            Ok(owner) => owner,
            Err(error) => {
                tracing::warn!(
                    "skipped result of /root/{}: owner {} unavailable: {}",
                    edge.name,
                    edge.session_id,
                    error
                );
                return;
            }
        };
        let (status, payload) = match outcome {
            AgentOutcome::Succeeded => {
                let text = Self::bounded_answer(answer);
                (ChildStatus::Completed(Some(text.clone())), Some(text))
            }
            AgentOutcome::Failed { message } => (
                ChildStatus::Errored(message.clone()),
                Some(format!("Agent errored: {message}")),
            ),
            AgentOutcome::Cancelled => (ChildStatus::Interrupted, None),
        };
        if let Err(error) = owner.with_children(self, |children| {
            if let Some(entry) = children.get_mut(&edge.name) {
                entry.status = status;
            }
        }) {
            tracing::warn!(
                "failed to record status of /root/{}: {}",
                edge.name,
                error
            );
        }
        let Some(payload) = payload else {
            tracing::info!(
                "skipped result of /root/{}: interrupted",
                edge.name
            );
            return;
        };
        let mail = AgentMail::builder()
            .author(AgentPath::child(&edge.name))
            .recipient(AgentPath::root())
            .kind(MailKind::Result)
            .payload(payload)
            .build();
        let kernel = self.clone();
        let owner_id = edge.session_id.clone();
        let name = edge.name.clone();
        let queued = owner.background.spawn(async move {
            match kernel
                .deliver_mail(
                    MailDelivery::builder()
                        .recipient(owner_id.clone())
                        .mail(mail)
                        .build(),
                )
                .await
            {
                Ok(placement) => tracing::info!(
                    "delivered result of /root/{} to owner {} as {:?}",
                    name,
                    owner_id,
                    placement
                ),
                Err(error) => tracing::warn!(
                    "failed result delivery of /root/{} to owner {}: {}",
                    name,
                    owner_id,
                    error
                ),
            }
        });
        if let Err(error) = queued {
            tracing::info!("skipped result of /root/{}: {}", edge.name, error);
        }
    }

    /// Bounds an answer to the mail limit with a hint that survives delivery.
    fn bounded_answer(text: &str) -> String {
        if text.trim().is_empty() {
            return "(no output)".to_string();
        }
        cut_bytes(text.to_string(), ::tools::TOOL_TEXT_BYTES, ANSWER_TRUNCATED)
    }
}

/// The `<subagents>` roster stated in an owner's context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::runtime) struct SubagentRosterNote {
    pub(in crate::runtime) paths: Vec<AgentPath>,
    /// Turn the note belongs to, so the rebuilt note keeps its live identity.
    pub(in crate::runtime) turn_id: TurnId,
}

impl ClawcodeEntry for SubagentRosterNote {
    /// Also the type of the hidden extension message the note becomes.
    const CUSTOM_TYPE: &'static str = protocol::SUBAGENT_ROSTER_TYPE;
}

impl StatedNote for SubagentRosterNote {
    type State = Vec<AgentPath>;
    const ID_PREFIX: &'static str = "subagent-roster";

    /// Wraps the children stated before `turn_id`.
    fn new(paths: Vec<AgentPath>, turn_id: TurnId) -> Self {
        Self { paths, turn_id }
    }

    /// Returns the stated child paths.
    fn state(&self) -> &Vec<AgentPath> {
        &self.paths
    }

    /// Returns the Turn the note belongs to.
    fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Renders Codex `format_agent_paths`: at most eight entries and 1024 bytes.
    fn render(&self) -> String {
        const MAX_AGENTS: usize = 8;
        const MAX_BYTES: usize = 1_024;
        let mut used = "<subagents>\n</subagents>".len();
        let mut lines = String::new();
        for path in self.paths.iter().take(MAX_AGENTS) {
            let line = format!("  <agent name=\"{path}\" />\n");
            if used.saturating_add(line.len()) > MAX_BYTES {
                break;
            }
            used = used.saturating_add(line.len());
            lines.push_str(&line);
        }
        format!("<subagents>\n{lines}</subagents>")
    }
}
