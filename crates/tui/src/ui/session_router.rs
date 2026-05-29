//! Multi-session routing above the single-session AppState reducer.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agent_client_protocol::schema::{
    SessionId, SessionNotification, SessionUpdate, StopReason, ToolCallUpdate,
};

use crate::ui::agent_navigation::AgentNavigationState;
use crate::ui::agent_picker::AgentPickerPanelState;
use crate::ui::composer::Composer;
use crate::ui::state::AppState;
use crate::ui::theme::Theme;
use crate::ui::view::ViewState;

/// Error returned when selecting an agent session fails.
#[derive(Debug)]
pub(crate) enum SelectAgentSessionError {
    NotSelectable(SessionId),
}

impl std::fmt::Display for SelectAgentSessionError {
    /// Format selection errors for user-visible error reporting.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectAgentSessionError::NotSelectable(session_id) => {
                write!(f, "agent session is not selectable: {session_id}")
            }
        }
    }
}

impl std::error::Error for SelectAgentSessionError {}

/// Top-level TUI state for routing multiple ACP sessions.
#[derive(Debug, typed_builder::TypedBuilder)]
pub(crate) struct SessionRouterState {
    /// Session currently shown and used for prompt submit.
    active_session_id: SessionId,
    /// Working directory used when lazily constructing session states.
    cwd: PathBuf,
    /// Model label copied into lazily constructed session states.
    model_label: String,
    /// Render theme copied into lazily constructed session states.
    theme: Theme,
    /// Per-session transcript reducers.
    states: HashMap<SessionId, AppState>,
    /// Sessions whose persisted history has been loaded or replayed.
    #[builder(default)]
    loaded_sessions: HashSet<SessionId>,
    /// Per-session viewport state.
    #[builder(default)]
    view_snapshots: HashMap<SessionId, ViewState>,
    /// Per-session composer drafts.
    #[builder(default)]
    composer_snapshots: HashMap<SessionId, Composer>,
    /// Agent picker metadata and ordering.
    agent_navigation: AgentNavigationState,
    /// Inline picker focus and selection state.
    #[builder(default)]
    agent_picker: AgentPickerPanelState,
}

impl SessionRouterState {
    /// Create a router with a root AppState and root picker entry.
    pub(crate) fn new(
        root_session_id: SessionId,
        cwd: PathBuf,
        model_label: String,
        theme: Theme,
    ) -> Self {
        let root_state = AppState::new_with_theme(
            root_session_id.clone(),
            cwd.clone(),
            model_label.clone(),
            theme.clone(),
        );
        let mut states = HashMap::new();
        states.insert(root_session_id.clone(), root_state);
        let mut loaded_sessions = HashSet::new();
        loaded_sessions.insert(root_session_id.clone());
        SessionRouterState::builder()
            .active_session_id(root_session_id.clone())
            .cwd(cwd)
            .model_label(model_label)
            .theme(theme)
            .states(states)
            .loaded_sessions(loaded_sessions)
            .agent_navigation(AgentNavigationState::new(root_session_id))
            .build()
    }

    /// Return the currently active ACP session id.
    pub(crate) fn active_session_id(&self) -> &SessionId {
        &self.active_session_id
    }

    /// Return the router working directory for ACP load-session fallback.
    pub(crate) fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    /// Return the active AppState.
    pub(crate) fn active_state(&self) -> &AppState {
        self.states
            .get(&self.active_session_id)
            .expect("active session state must exist")
    }

    /// Return the mutable active AppState.
    pub(crate) fn active_state_mut(&mut self) -> &mut AppState {
        self.states
            .get_mut(&self.active_session_id)
            .expect("active session state must exist")
    }

    /// Return a session state by id.
    #[cfg(test)]
    pub(crate) fn state_for(
        &self,
        session_id: &SessionId,
    ) -> Option<&AppState> {
        self.states.get(session_id)
    }

    /// Return whether a session state has already been created.
    #[cfg(test)]
    pub(crate) fn has_state(&self, session_id: &SessionId) -> bool {
        self.states.contains_key(session_id)
    }

    /// Return whether a session's persisted history has already been loaded.
    pub(crate) fn is_session_loaded(&self, session_id: &SessionId) -> bool {
        self.loaded_sessions.contains(session_id)
    }

    /// Mark a session as loaded after ACP load-session replay completes.
    pub(crate) fn mark_session_loaded(&mut self, session_id: SessionId) {
        self.loaded_sessions.insert(session_id);
    }

    /// Return immutable agent navigation state.
    pub(crate) fn agent_navigation(&self) -> &AgentNavigationState {
        &self.agent_navigation
    }

    /// Return mutable agent navigation state.
    #[cfg(test)]
    pub(crate) fn agent_navigation_mut(&mut self) -> &mut AgentNavigationState {
        &mut self.agent_navigation
    }

    /// Open the inline agent picker.
    pub(crate) fn open_agent_picker(&mut self) {
        self.agent_picker.open(self.agent_navigation.len());
    }

    /// Close the inline agent picker.
    pub(crate) fn close_agent_picker(&mut self) {
        self.agent_picker.close();
    }

    /// Return whether the inline picker is focused.
    pub(crate) fn is_agent_picker_focused(&self) -> bool {
        self.agent_picker.is_focused()
    }

    /// Return the height the picker should reserve in the bottom input area.
    pub(crate) fn agent_picker_height(&self) -> u16 {
        if self.agent_picker.is_visible() {
            self.agent_navigation.len().clamp(1, 5) as u16
        } else {
            0
        }
    }

    /// Move picker selection to the previous agent.
    pub(crate) fn move_agent_picker_previous(&mut self) {
        self.agent_picker.move_previous(self.agent_navigation.len());
    }

    /// Move picker selection to the next agent.
    pub(crate) fn move_agent_picker_next(&mut self) {
        self.agent_picker.move_next(self.agent_navigation.len());
    }

    /// Return the currently selected picker entry.
    pub(crate) fn selected_agent_session_id(&self) -> Option<&SessionId> {
        self.agent_navigation
            .entry_at(self.agent_picker.selected_index())
            .map(|entry| entry.session_id())
    }

    /// Return the selected picker index for rendering.
    pub(crate) fn agent_picker_selected_index(&self) -> usize {
        self.agent_picker.selected_index()
    }

    /// Return the active agent label for status rendering.
    pub(crate) fn active_agent_label(&self) -> String {
        self.agent_navigation
            .label_for_session(&self.active_session_id)
    }

    /// Route one ACP session notification to metadata state or the matching AppState.
    pub(crate) fn apply_session_notification(
        &mut self,
        notification: SessionNotification,
    ) {
        if let Some(patch) = Self::subagent_metadata_patch(&notification.update)
        {
            self.apply_agent_statuses(&patch);
            // Snapshots describe the root session tree. Lazy-loading a child session can also
            // emit a child-rooted snapshot, which must not replace the global picker metadata.
            let is_child_snapshot = patch.event
                == protocol::AgentUiEventKind::Snapshot
                && !self
                    .agent_navigation
                    .is_root_session(&notification.session_id);
            if !is_child_snapshot {
                self.agent_navigation.apply_patch(patch);
            }
            return;
        }

        let session_id = notification.session_id.clone();
        self.ensure_state(session_id.clone());
        if let Some(state) = self.states.get_mut(&session_id) {
            state.apply_session_update(notification);
        }
    }

    /// Mirror non-root lifecycle metadata into per-session status state.
    fn apply_agent_statuses(&mut self, patch: &protocol::AgentUiMetadataPatch) {
        for metadata in &patch.agents {
            if patch.event == protocol::AgentUiEventKind::Snapshot
                && matches!(
                    metadata.status,
                    protocol::AgentStatus::PendingInit
                        | protocol::AgentStatus::Running
                )
            {
                continue;
            }
            let session_id = SessionId::from(&metadata.session_id);
            // Root metadata defaults to running for picker availability, so only child sessions
            // should drive the status line through metadata.
            if self.agent_navigation.is_root_session(&session_id) {
                continue;
            }
            self.ensure_state(session_id.clone());
            if let Some(state) = self.states.get_mut(&session_id) {
                state.apply_agent_status(metadata.status.clone());
            }
        }
    }

    /// Select a visible agent session and make it the active TUI context.
    pub(crate) fn select_agent_session(
        &mut self,
        target_session_id: SessionId,
        current_view: &mut ViewState,
        current_composer: &mut Composer,
    ) -> Result<(), SelectAgentSessionError> {
        if target_session_id == self.active_session_id {
            return Ok(());
        }
        let selectable = self
            .agent_navigation
            .ordered_entries()
            .into_iter()
            .any(|entry| entry.session_id() == &target_session_id);
        if !selectable {
            return Err(SelectAgentSessionError::NotSelectable(
                target_session_id,
            ));
        }

        self.view_snapshots
            .insert(self.active_session_id.clone(), current_view.clone());
        self.composer_snapshots
            .insert(self.active_session_id.clone(), current_composer.clone());
        self.ensure_state(target_session_id.clone());
        self.active_session_id = target_session_id.clone();
        *current_view = self
            .view_snapshots
            .remove(&target_session_id)
            .unwrap_or_default();
        *current_composer = self
            .composer_snapshots
            .remove(&target_session_id)
            .unwrap_or_default();
        Ok(())
    }

    /// Record a prompt completion for its owning session.
    pub(crate) fn finish_prompt_for_session(
        &mut self,
        session_id: &SessionId,
        stop_reason: StopReason,
    ) {
        self.ensure_state(session_id.clone());
        if let Some(state) = self.states.get_mut(session_id) {
            state.finish_prompt(stop_reason);
        }
    }

    /// Record an error for its owning session.
    pub(crate) fn set_error_for_session(
        &mut self,
        session_id: &SessionId,
        message: String,
    ) {
        self.ensure_state(session_id.clone());
        if let Some(state) = self.states.get_mut(session_id) {
            state.set_error(message);
        }
    }

    /// Return a saved composer draft for tests and future session restoration.
    #[cfg(test)]
    pub(crate) fn composer_snapshot_text(
        &self,
        session_id: &SessionId,
    ) -> Option<&str> {
        self.composer_snapshots.get(session_id).map(Composer::text)
    }

    /// Ensure a session has an AppState so inactive output is never dropped.
    fn ensure_state(&mut self, session_id: SessionId) {
        self.states.entry(session_id.clone()).or_insert_with(|| {
            AppState::new_with_theme(
                session_id,
                self.cwd.clone(),
                self.model_label.clone(),
                self.theme.clone(),
            )
        });
    }

    /// Extract clawcode subagent metadata from a metadata-only ToolCallUpdate.
    fn subagent_metadata_patch(
        update: &SessionUpdate,
    ) -> Option<protocol::AgentUiMetadataPatch> {
        let SessionUpdate::ToolCallUpdate(ToolCallUpdate {
            meta: Some(meta),
            ..
        }) = update
        else {
            return None;
        };
        let payload = meta.get("clawcode")?.get("subagents")?.clone();
        serde_json::from_value(payload).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        ContentBlock, ContentChunk, TextContent, ToolCallId,
        ToolCallUpdateFields,
    };

    /// Build a metadata update for one child agent.
    fn metadata_update_for_child(
        session_id: &str,
        nickname: &str,
        role: &str,
    ) -> SessionUpdate {
        let metadata = protocol::AgentUiMetadata::builder()
            .session_id(protocol::SessionId::from(session_id.to_string()))
            .parent_session_id(protocol::SessionId::from("root-session"))
            .agent_path(protocol::AgentPath::root().join("inspect"))
            .nickname(nickname.to_string())
            .role(role.to_string())
            .status(protocol::AgentStatus::Running)
            .is_root(false)
            .build();
        let patch = protocol::AgentUiMetadataPatch::builder()
            .version(1)
            .event(protocol::AgentUiEventKind::Upsert)
            .agents(vec![metadata])
            .build();
        let meta = serde_json::json!({
            "clawcode": {
                "subagents": patch,
            }
        })
        .as_object()
        .cloned()
        .expect("metadata root should be an object");
        SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new(
                ToolCallId::new("clawcode-subagents"),
                ToolCallUpdateFields::default(),
            )
            .meta(meta),
        )
    }

    /// Build a snapshot metadata update for one child agent.
    fn snapshot_update_for_child(session_id: &str) -> SessionUpdate {
        let metadata = protocol::AgentUiMetadata::builder()
            .session_id(protocol::SessionId::from(session_id.to_string()))
            .parent_session_id(protocol::SessionId::from("root-session"))
            .agent_path(protocol::AgentPath::root().join("inspect"))
            .status(protocol::AgentStatus::Running)
            .is_root(false)
            .build();
        let patch = protocol::AgentUiMetadataPatch::builder()
            .version(1)
            .event(protocol::AgentUiEventKind::Snapshot)
            .agents(vec![metadata])
            .build();
        let meta = serde_json::json!({
            "clawcode": {
                "subagents": patch,
            }
        })
        .as_object()
        .cloned()
        .expect("metadata root should be an object");
        SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new(
                ToolCallId::new("clawcode-subagents"),
                ToolCallUpdateFields::default(),
            )
            .meta(meta),
        )
    }

    /// Verifies inactive session notifications are retained instead of ignored.
    #[test]
    fn router_keeps_inactive_session_notifications() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );

        router.apply_session_notification(SessionNotification::new(
            child.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new("child output")),
            )),
        ));

        assert_eq!(router.active_session_id(), &root);
        assert!(router.state_for(&child).unwrap().transcript().iter().any(
            |entry| {
                entry
                    .text_cell()
                    .is_some_and(|cell| cell.text().contains("child output"))
            }
        ));
    }

    /// Verifies metadata-only updates do not create visible tool cells.
    #[test]
    fn router_consumes_subagent_metadata_without_tool_cell() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );

        let update =
            metadata_update_for_child("child-session", "finder", "worker");
        router
            .apply_session_notification(SessionNotification::new(root, update));

        assert_eq!(router.active_state().transcript().len(), 0);
        assert_eq!(router.agent_navigation().ordered_entries().len(), 2);
        assert!(router.has_state(&child));
        assert!(!router.is_session_loaded(&child));
    }

    /// Verifies explicit load completion marks a metadata-created child as hydrated.
    #[test]
    fn router_tracks_loaded_sessions_separately_from_created_state() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root,
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let update =
            metadata_update_for_child("child-session", "finder", "worker");
        router.apply_session_notification(SessionNotification::new(
            SessionId::new("root-session"),
            update,
        ));

        router.mark_session_loaded(child.clone());

        assert!(router.is_session_loaded(&child));
    }

    /// Verifies restored snapshots do not mark an idle child context as running.
    #[test]
    fn restored_running_snapshot_does_not_drive_child_status_line() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let update = snapshot_update_for_child("child-session");
        router.apply_session_notification(SessionNotification::new(
            root.clone(),
            update,
        ));
        router.mark_session_loaded(child.clone());

        router
            .select_agent_session(
                child,
                &mut ViewState::default(),
                &mut Composer::default(),
            )
            .expect("select child");

        assert_eq!(router.active_state().top_status_line(), "idle");
    }

    /// Verifies selecting an unseen child preserves root draft and activates the child state.
    #[test]
    fn selecting_unseen_child_preserves_root_composer_snapshot() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let update =
            metadata_update_for_child("child-session", "finder", "worker");
        router.apply_session_notification(SessionNotification::new(
            root.clone(),
            update,
        ));
        let mut view = ViewState::default();
        let mut composer = Composer::default();
        composer.insert_str("root draft");

        router
            .select_agent_session(child.clone(), &mut view, &mut composer)
            .expect("select child");

        assert_eq!(router.active_session_id(), &child);
        assert!(composer.is_empty());
        assert_eq!(router.composer_snapshot_text(&root), Some("root draft"));
    }

    /// Verifies reopening `/agent` preserves the picker's last selected row.
    #[test]
    fn opening_agent_picker_preserves_last_picker_selection() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let update =
            metadata_update_for_child("child-session", "finder", "worker");
        router
            .apply_session_notification(SessionNotification::new(root, update));
        let mut view = ViewState::default();
        let mut composer = Composer::default();
        router.open_agent_picker();
        router.move_agent_picker_next();
        assert_eq!(router.selected_agent_session_id(), Some(&child));
        router
            .select_agent_session(
                router
                    .selected_agent_session_id()
                    .expect("selected session")
                    .clone(),
                &mut view,
                &mut composer,
            )
            .expect("select child");
        router.close_agent_picker();

        router.open_agent_picker();

        assert_eq!(router.selected_agent_session_id(), Some(&child));
    }

    /// Verifies child-session snapshots do not erase the active child label after switching.
    #[test]
    fn child_snapshot_after_switch_preserves_active_agent_label() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let child_update =
            metadata_update_for_child("child-session", "finder", "worker");
        router.apply_session_notification(SessionNotification::new(
            root.clone(),
            child_update,
        ));
        router
            .select_agent_session(
                child.clone(),
                &mut ViewState::default(),
                &mut Composer::default(),
            )
            .expect("select child");
        let child_root_snapshot = protocol::AgentUiMetadataPatch::builder()
            .version(1)
            .event(protocol::AgentUiEventKind::Snapshot)
            .agents(vec![
                protocol::AgentUiMetadata::builder()
                    .session_id(protocol::SessionId::from("child-session"))
                    .agent_path(protocol::AgentPath::root())
                    .status(protocol::AgentStatus::Running)
                    .is_root(true)
                    .build(),
            ])
            .build();
        let meta = serde_json::json!({
            "clawcode": {
                "subagents": child_root_snapshot,
            }
        })
        .as_object()
        .cloned()
        .expect("metadata root should be an object");

        router.apply_session_notification(SessionNotification::new(
            child,
            SessionUpdate::ToolCallUpdate(
                ToolCallUpdate::new(
                    ToolCallId::new("clawcode-subagents"),
                    ToolCallUpdateFields::default(),
                )
                .meta(meta),
            ),
        ));

        assert_eq!(router.active_agent_label(), "finder [worker]");
    }

    /// Verifies running subagent metadata drives the active child status line.
    #[test]
    fn running_subagent_metadata_updates_active_child_status_line() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        let update =
            metadata_update_for_child("child-session", "finder", "worker");
        router.apply_session_notification(SessionNotification::new(
            root.clone(),
            update,
        ));
        router
            .select_agent_session(
                child,
                &mut ViewState::default(),
                &mut Composer::default(),
            )
            .expect("select child");

        assert_eq!(router.active_state().top_status_line(), "running");
    }
}
