//! Inline model picker state and rendering helpers.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};

use crossterm::event::KeyCode;

use crate::acp::client::AcpClient;
use crate::ui::picker::{Picker, PickerAction, PickerState};
use crate::ui::session_router::{ModelOption, SessionRouterState};
use crate::ui::theme::Theme;

/// Focus and selection state for the inline `/model` picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModelPicker {
    state: PickerState,
}

impl Picker for ModelPicker {
    /// Returns immutable shared picker state.
    fn state(&self) -> &PickerState {
        &self.state
    }

    /// Returns mutable shared picker state.
    fn state_mut(&mut self) -> &mut PickerState {
        &mut self.state
    }
}

impl ModelPicker {
    /// Handles shared key behavior and maps submitted rows to model ids.
    pub(crate) fn handle_key_for_models(
        &mut self,
        code: KeyCode,
        models: &[ModelOption],
    ) -> Option<String> {
        match self.handle_key(code, models.len()) {
            PickerAction::Submit(index) => {
                models.get(index).map(|model| model.id().to_string())
            }
            PickerAction::None => None,
        }
    }
}

/// Handles a focused model picker key event and switches the selected model.
pub(crate) async fn handle_model_picker_key(
    client: &AcpClient,
    router: &mut SessionRouterState,
    code: KeyCode,
) {
    if let Some(model_id) = router.handle_model_picker_key(code) {
        switch_model(client, router, model_id).await;
    }
}

/// Switches the root session model and updates local TUI state on success.
pub(crate) async fn switch_model(
    client: &AcpClient,
    router: &mut SessionRouterState,
    requested_model: String,
) {
    let session_id = router.active_session_id().clone();
    match client.set_model(session_id, requested_model.clone()).await {
        Ok(()) => {
            router.set_active_model_label(requested_model.clone());
            router.close_model_picker();
            router.active_state_mut().add_system_message(format!(
                "Model switched to {requested_model}"
            ));
        }
        Err(error) => {
            router
                .active_state_mut()
                .set_error(format!("failed to switch model: {error}"));
        }
    }
}

/// Renders the inline model picker below the composer.
pub(crate) fn render_model_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    router: &SessionRouterState,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let current = router.active_state().model_label();
    let selected_index = router.model_picker_selected_index();
    let lines = router
        .available_models()
        .iter()
        .enumerate()
        .take(usize::from(area.height))
        .map(|(index, model)| {
            let is_selected = index == selected_index;
            let is_current = model.id() == current;
            let marker = if is_selected { ">" } else { " " };
            let current = if is_current { " current" } else { "" };
            let style = if is_selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(vec![
                Span::styled(format!("{marker} "), style),
                Span::styled(model.name(), style),
                Span::styled(
                    format!(" {}{}", model.id(), current),
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
