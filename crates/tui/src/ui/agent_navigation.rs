//! Agent picker ordering, labels, and status state.

use std::collections::HashMap;

use agent_client_protocol::schema::SessionId;

/// Display status used by the TUI agent picker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentPickerStatus {
    Pending,
    Running,
    Completed,
    Errored,
    Closed,
    #[default]
    Unknown,
}

/// One selectable agent row in the picker.
#[derive(Clone, Debug, PartialEq, Eq, typed_builder::TypedBuilder)]
pub(crate) struct AgentPickerEntry {
    /// ACP session id used for prompt routing and transcript switching.
    session_id: SessionId,
    /// Parent session id for non-root agents.
    #[builder(default, setter(strip_option))]
    parent_session_id: Option<SessionId>,
    /// Canonical runtime path.
    #[builder(default, setter(strip_option))]
    agent_path: Option<String>,
    /// Human-friendly nickname.
    #[builder(default, setter(strip_option))]
    nickname: Option<String>,
    /// Runtime role name.
    #[builder(default, setter(strip_option))]
    role: Option<String>,
    /// Latest display status.
    #[builder(default)]
    status: AgentPickerStatus,
    /// True for the main/root session.
    is_root: bool,
}

impl AgentPickerEntry {
    /// Returns the ACP session id for this picker entry.
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the latest picker status.
    pub(crate) fn status(&self) -> AgentPickerStatus {
        self.status
    }

    /// Builds the label shown in the picker.
    pub(crate) fn label(&self) -> String {
        if self.is_root {
            return "Main [default]".to_string();
        }
        match (self.nickname.as_deref(), self.role.as_deref()) {
            (Some(nickname), Some(role)) => format!("{nickname} [{role}]"),
            (Some(nickname), None) => nickname.to_string(),
            (None, Some(role)) => format!("Agent [{role}]"),
            (None, None) => self
                .agent_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
                .filter(|name| !name.is_empty())
                .unwrap_or("Agent")
                .to_string(),
        }
    }

    /// Merge newer metadata while preserving existing non-empty optional fields.
    fn merge(&mut self, incoming: AgentPickerEntry) {
        self.status = incoming.status;
        self.is_root = self.is_root || incoming.is_root;
        if incoming.parent_session_id.is_some() {
            self.parent_session_id = incoming.parent_session_id;
        }
        if incoming.agent_path.is_some() {
            self.agent_path = incoming.agent_path;
        }
        if incoming.nickname.is_some() {
            self.nickname = incoming.nickname;
        }
        if incoming.role.is_some() {
            self.role = incoming.role;
        }
    }
}

impl From<protocol::AgentUiMetadata> for AgentPickerEntry {
    /// Convert protocol metadata into a TUI picker entry.
    fn from(metadata: protocol::AgentUiMetadata) -> Self {
        let session_id = SessionId::from(metadata.session_id);
        let mut entry = AgentPickerEntry::builder()
            .session_id(session_id)
            .agent_path(metadata.agent_path.to_string())
            .status(metadata.status.into())
            .is_root(metadata.is_root)
            .build();
        // typed-builder optional setters change the builder type, so copy optional metadata after
        // construction to keep conversion straightforward and stable.
        entry.parent_session_id = metadata.parent_session_id.map(SessionId::from);
        entry.nickname = metadata.nickname;
        entry.role = metadata.role;
        entry
    }
}

impl From<protocol::AgentStatus> for AgentPickerStatus {
    /// Convert kernel status into picker status tokens.
    fn from(status: protocol::AgentStatus) -> Self {
        match status {
            protocol::AgentStatus::PendingInit => Self::Pending,
            protocol::AgentStatus::Running => Self::Running,
            protocol::AgentStatus::Completed { .. } => Self::Completed,
            protocol::AgentStatus::Errored { .. } => Self::Errored,
            protocol::AgentStatus::Interrupted | protocol::AgentStatus::Shutdown => Self::Closed,
            protocol::AgentStatus::NotFound => Self::Unknown,
        }
    }
}

/// Stable first-seen order and metadata for the agent picker.
#[derive(Clone, Debug)]
pub(crate) struct AgentNavigationState {
    root_session_id: SessionId,
    agents: HashMap<SessionId, AgentPickerEntry>,
    order: Vec<SessionId>,
}

impl AgentNavigationState {
    /// Create navigation state with the main agent pinned as the first entry.
    pub(crate) fn new(root_session_id: SessionId) -> Self {
        let root_entry = AgentPickerEntry::builder()
            .session_id(root_session_id.clone())
            .agent_path(protocol::AgentPath::root().to_string())
            .status(AgentPickerStatus::Running)
            .is_root(true)
            .build();
        let mut agents = HashMap::new();
        agents.insert(root_session_id.clone(), root_entry);
        Self {
            root_session_id: root_session_id.clone(),
            agents,
            order: vec![root_session_id],
        }
    }

    /// Upsert one picker entry while preserving first-seen ordering.
    pub(crate) fn upsert(&mut self, mut entry: AgentPickerEntry) {
        if entry.session_id == self.root_session_id {
            entry.is_root = true;
        }
        if let Some(existing) = self.agents.get_mut(&entry.session_id) {
            existing.merge(entry);
            return;
        }
        if entry.is_root {
            self.order
                .retain(|session_id| session_id != &entry.session_id);
            self.order.insert(0, entry.session_id.clone());
        } else {
            self.order.push(entry.session_id.clone());
        }
        self.agents.insert(entry.session_id.clone(), entry);
        self.ensure_root_first();
    }

    /// Apply a protocol metadata patch to the navigation state.
    pub(crate) fn apply_patch(&mut self, patch: protocol::AgentUiMetadataPatch) {
        match patch.event {
            protocol::AgentUiEventKind::Snapshot => self.apply_snapshot(patch.agents),
            protocol::AgentUiEventKind::Upsert | protocol::AgentUiEventKind::Status => {
                for metadata in patch.agents {
                    self.upsert(metadata.into());
                }
            }
        }
    }

    /// Return picker entries in stable display order.
    pub(crate) fn ordered_entries(&self) -> Vec<&AgentPickerEntry> {
        self.order
            .iter()
            .filter_map(|session_id| self.agents.get(session_id))
            .collect()
    }

    /// Return the entry at the display index.
    pub(crate) fn entry_at(&self, index: usize) -> Option<&AgentPickerEntry> {
        let session_id = self.order.get(index)?;
        self.agents.get(session_id)
    }

    /// Return the label for the active session.
    pub(crate) fn label_for_session(&self, session_id: &SessionId) -> String {
        self.agents
            .get(session_id)
            .map(AgentPickerEntry::label)
            .unwrap_or_else(|| session_id.to_string())
    }

    /// Return whether the session owns the root-scoped navigation snapshot.
    pub(crate) fn is_root_session(&self, session_id: &SessionId) -> bool {
        session_id == &self.root_session_id
    }

    /// Return the number of displayable picker entries.
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    /// Keep the root session available and pinned to the first display slot.
    fn ensure_root_first(&mut self) {
        if !self.agents.contains_key(&self.root_session_id) {
            let root_entry = AgentPickerEntry::builder()
                .session_id(self.root_session_id.clone())
                .agent_path(protocol::AgentPath::root().to_string())
                .status(AgentPickerStatus::Running)
                .is_root(true)
                .build();
            self.agents.insert(self.root_session_id.clone(), root_entry);
        }
        self.order
            .retain(|session_id| self.agents.contains_key(session_id));
        self.order
            .retain(|session_id| session_id != &self.root_session_id);
        self.order.insert(0, self.root_session_id.clone());
    }

    /// Replace picker metadata with an authoritative live-agent snapshot.
    fn apply_snapshot(&mut self, agents: Vec<protocol::AgentUiMetadata>) {
        // A snapshot reflects the registry's current live sessions. Replacing the picker metadata
        // keeps closed agents out of the switch list while the router can still retain transcript
        // state for sessions it has already loaded.
        self.agents.clear();
        self.order.clear();
        for metadata in agents {
            self.upsert(metadata.into());
        }
        self.ensure_root_first();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a child entry for status updates and ordering tests.
    fn child_entry(
        session_id: SessionId,
        parent_session_id: SessionId,
        path: &str,
    ) -> AgentPickerEntry {
        AgentPickerEntry::builder()
            .session_id(session_id)
            .parent_session_id(parent_session_id)
            .agent_path(path.to_string())
            .status(AgentPickerStatus::Running)
            .is_root(false)
            .build()
    }

    /// Build a status-only entry for snapshot and merge-path tests.
    fn status_only_entry(session_id: SessionId, status: AgentPickerStatus) -> AgentPickerEntry {
        AgentPickerEntry::builder()
            .session_id(session_id)
            .status(status)
            .is_root(false)
            .build()
    }

    /// Verifies root is always first and labeled as Main [default].
    #[test]
    fn root_entry_is_first_and_labeled_main_default() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut state = AgentNavigationState::new(root.clone());

        state.upsert(
            AgentPickerEntry::builder()
                .session_id(child)
                .parent_session_id(root.clone())
                .agent_path("/root/inspect".to_string())
                .nickname("finder".to_string())
                .role("worker".to_string())
                .status(AgentPickerStatus::Running)
                .is_root(false)
                .build(),
        );

        let entries = state.ordered_entries();
        assert_eq!(entries[0].session_id(), &root);
        assert_eq!(entries[0].label(), "Main [default]");
        assert_eq!(entries[1].label(), "finder [worker]");
    }

    /// Verifies status updates do not change first-seen ordering.
    #[test]
    fn status_update_preserves_first_seen_order() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut state = AgentNavigationState::new(root.clone());
        state.upsert(child_entry(child.clone(), root, "/root/inspect"));
        state.upsert(status_only_entry(
            child.clone(),
            AgentPickerStatus::Completed,
        ));

        assert_eq!(state.ordered_entries()[1].session_id(), &child);
        assert_eq!(
            state.ordered_entries()[1].status(),
            AgentPickerStatus::Completed
        );
    }

    /// Verifies snapshots remove child entries that are no longer live.
    #[test]
    fn snapshot_removes_missing_child_entries() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut state = AgentNavigationState::new(root.clone());
        state.upsert(status_only_entry(child.clone(), AgentPickerStatus::Closed));
        let patch = protocol::AgentUiMetadataPatch::builder()
            .version(1)
            .event(protocol::AgentUiEventKind::Snapshot)
            .agents(vec![
                protocol::AgentUiMetadata::builder()
                    .session_id(protocol::SessionId::from("root-session"))
                    .agent_path(protocol::AgentPath::root())
                    .status(protocol::AgentStatus::Running)
                    .is_root(true)
                    .build(),
            ])
            .build();

        state.apply_patch(patch);

        assert!(
            !state
                .ordered_entries()
                .iter()
                .any(|entry| entry.session_id() == &child)
        );
        assert_eq!(state.ordered_entries()[0].session_id(), &root);
    }
}
