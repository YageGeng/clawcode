//! Inline agent picker state and rendering helpers.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};

use agent_client_protocol::schema::SessionId;
use crossterm::event::KeyCode;

use crate::acp::client::AcpClient;
use crate::ui::agent_navigation::AgentNavigationState;
use crate::ui::composer::Composer;
use crate::ui::picker::{Picker, PickerAction, PickerState};
use crate::ui::session_router::SessionRouterState;
use crate::ui::theme::Theme;
use crate::ui::view::ViewState;

/// Focus and selection state for the inline `/agent` picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentPicker {
    state: PickerState,
}

impl Picker for AgentPicker {
    /// Returns immutable shared picker state.
    fn state(&self) -> &PickerState {
        &self.state
    }

    /// Returns mutable shared picker state.
    fn state_mut(&mut self) -> &mut PickerState {
        &mut self.state
    }
}

impl AgentPicker {
    /// Handles shared key behavior and maps submitted rows to agent sessions.
    pub(crate) fn handle_key_for_navigation(
        &mut self,
        code: KeyCode,
        navigation: &AgentNavigationState,
    ) -> Option<SessionId> {
        match self.handle_key(code, navigation.len()) {
            PickerAction::Submit(index) => navigation
                .entry_at(index)
                .map(|entry| entry.session_id().clone()),
            PickerAction::None => None,
        }
    }
}

/// Handles a focused agent picker key event and applies selected-session state.
pub(crate) async fn handle_agent_picker_key(
    client: &AcpClient,
    router: &mut SessionRouterState,
    view: &mut ViewState,
    composer: &mut Composer,
    code: KeyCode,
) -> anyhow::Result<()> {
    if let Some(target_session_id) = router.handle_agent_picker_key(code) {
        if !ensure_loaded_agent_session(client, router, &target_session_id)
            .await
        {
            return Ok(());
        }
        router.select_agent_session(target_session_id, view, composer)?;
        router.close_agent_picker();
    }
    Ok(())
}

/// Try to load a target agent session before switching to it.
async fn ensure_loaded_agent_session(
    client: &AcpClient,
    router: &mut SessionRouterState,
    target_session_id: &SessionId,
) -> bool {
    if router.is_session_loaded(target_session_id) {
        return true;
    }
    if let Err(error) = client
        .load_session(target_session_id.clone(), router.cwd().clone())
        .await
    {
        router
            .active_state_mut()
            .set_error(format!("failed to load agent session: {error}"));
        return false;
    }
    router.mark_session_loaded(target_session_id.clone());
    true
}

/// Renders the inline agent picker below the composer.
pub(crate) fn render_agent_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    router: &SessionRouterState,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let entries = router.agent_navigation().ordered_entries();
    let selected_index = router.agent_picker_selected_index();
    let lines = entries
        .into_iter()
        .enumerate()
        .take(usize::from(area.height))
        .map(|(index, entry)| {
            let is_selected = index == selected_index;
            let is_active = entry.session_id() == router.active_session_id();
            let marker = if is_selected { ">" } else { " " };
            let current = if is_active { " current" } else { "" };
            let status = entry.status().symbol();
            let style = if is_selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(vec![
                Span::styled(format!("{marker} {status} "), style),
                Span::styled(entry.label(), style),
                Span::styled(
                    current.to_string(),
                    Style::default().fg(theme.muted),
                ),
            ])
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.surface)),
        area,
    );
}
