//! The agent tree one tool call acts in: `/root` and its named children.

use std::collections::BTreeMap;
use std::sync::Arc;

use protocol::{AgentName, AgentPath, SessionId};

use super::roster::{ChildEntry, ChildStatus};
use crate::runtime::{Kernel, KernelError, Session};

/// A resolved tool target.
pub(in crate::runtime) enum AgentTarget {
    /// The owner agent.
    Root,
    /// One named child.
    Child {
        path: AgentPath,
        session_id: SessionId,
    },
}

/// Borrows the Kernel with the caller's owner, so children read the owner's roster.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct AgentTree<'a> {
    kernel: &'a Kernel,
    owner_id: SessionId,
    owner: Arc<Session>,
    caller: AgentPath,
}

impl<'a> AgentTree<'a> {
    /// Resolves the tree of the calling Session; a child acts in its owner's tree.
    pub(in crate::runtime) fn of(
        kernel: &'a Kernel,
        caller_id: &SessionId,
        caller: &Arc<Session>,
    ) -> Result<Self, KernelError> {
        Ok(match &caller.ownership {
            Some(edge) => Self::builder()
                .kernel(kernel)
                .owner_id(edge.session_id.clone())
                .owner(kernel.session(&edge.session_id)?)
                .caller(AgentPath::child(&edge.name))
                .build(),
            None => Self::builder()
                .kernel(kernel)
                .owner_id(caller_id.clone())
                .owner(Arc::clone(caller))
                .caller(AgentPath::root())
                .build(),
        })
    }

    /// The owner Session that holds the roster and spawn admission lock.
    pub(in crate::runtime) fn owner(&self) -> &Arc<Session> {
        &self.owner
    }

    /// The owner's Session identifier.
    pub(in crate::runtime) fn owner_id(&self) -> &SessionId {
        &self.owner_id
    }

    /// The calling agent's canonical path.
    pub(in crate::runtime) fn caller(&self) -> &AgentPath {
        &self.caller
    }

    /// Runs `f` on the owner's roster.
    pub(in crate::runtime) fn with_children<R>(
        &self,
        f: impl FnOnce(&mut BTreeMap<AgentName, ChildEntry>) -> R,
    ) -> Result<R, KernelError> {
        self.owner.with_children(self.kernel, f)
    }

    /// Reports whether a live Session of this tree has an active Run.
    pub(in crate::runtime) fn is_running(
        &self,
        session_id: &SessionId,
    ) -> bool {
        self.kernel
            .session(session_id)
            .is_ok_and(|session| session.execution.is_running())
    }

    /// Counts a live child as running while its Run is active or before its
    /// first Run settled; a closed child never holds a slot.
    fn holds_slot(&self, entry: &ChildEntry) -> bool {
        self.kernel.session(&entry.session_id).is_ok_and(|child| {
            entry.status == ChildStatus::PendingInit
                || child.execution.is_running()
        })
    }

    /// Refuses another running child once `max_agents` children hold slots;
    /// an `exempt` child that already holds one is not counted against itself.
    pub(in crate::runtime) fn over_limit(
        &self,
        children: &BTreeMap<AgentName, ChildEntry>,
        exempt: Option<&str>,
    ) -> Option<String> {
        if exempt
            .and_then(|name| children.get(name))
            .is_some_and(|entry| self.holds_slot(entry))
        {
            return None;
        }
        let running = children
            .values()
            .filter(|entry| self.holds_slot(entry))
            .count();
        (running >= self.kernel.subagent_max_agents.get()).then(|| {
            format!(
                "agent limit reached: {running} agents are running; wait for one to finish."
            )
        })
    }

    /// Resolves a target like Codex: `/root`, `/root/<name>`, or `<name>`.
    ///
    /// The inner `Err` is correctable usage text for the model.
    pub(in crate::runtime) fn resolve(
        &self,
        target: &str,
    ) -> Result<Result<AgentTarget, String>, KernelError> {
        let path = match AgentPath::resolve(target) {
            Ok(path) => path,
            Err(text) => return Ok(Err(text)),
        };
        if path.is_root() {
            return Ok(Ok(AgentTarget::Root));
        }
        let session_id = self.with_children(|children| {
            children
                .get(path.name())
                .map(|entry| entry.session_id.clone())
        })?;
        Ok(match session_id {
            Some(session_id) => Ok(AgentTarget::Child { path, session_id }),
            None => Err(format!("live agent path `{path}` not found")),
        })
    }
}
