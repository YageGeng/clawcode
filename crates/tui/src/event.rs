//! Terminal and application event types.

use crossterm::event::{Event as CrosstermEvent, KeyEvent, KeyEventKind, MouseEventKind};

/// A normalized event type used by the local TUI layer.
#[derive(Debug, PartialEq, Eq)]
pub enum TuiEvent {
    /// Key input event.
    Key(KeyEvent),
    /// Paste event text.
    Paste(String),
    /// Terminal resize or terminal focus update event.
    Resize,
    /// Mouse wheel scroll toward older transcript content.
    ScrollUp,
    /// Mouse wheel scroll toward newer transcript content.
    ScrollDown,
    /// Timer tick event.
    Tick,
}

/// Maps a crossterm event into a TUI event if the event is relevant.
pub fn map_crossterm_event(event: CrosstermEvent) -> Option<TuiEvent> {
    match event {
        // Ignore key repeat and release events to keep one-shot composer and approval handling.
        CrosstermEvent::Key(key_event) if key_event.kind == KeyEventKind::Press => {
            Some(TuiEvent::Key(key_event))
        }
        CrosstermEvent::Paste(text) => Some(TuiEvent::Paste(text.replace('\r', "\n"))),
        CrosstermEvent::Mouse(mouse_event) => match mouse_event.kind {
            MouseEventKind::ScrollUp => Some(TuiEvent::ScrollUp),
            MouseEventKind::ScrollDown => Some(TuiEvent::ScrollDown),
            _ => None,
        },
        CrosstermEvent::Resize(_, _) => Some(TuiEvent::Resize),
        CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => Some(TuiEvent::Resize),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEvent, MouseEventKind,
    };

    /// Verifies key events are mapped to [`TuiEvent::Key`].
    #[test]
    fn key_event_maps_to_tui_event_key() {
        let key_event = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let event = map_crossterm_event(CrosstermEvent::Key(key_event));
        assert!(matches!(event, Some(TuiEvent::Key(_))));
    }

    /// Verifies non-press key events are ignored by the event mapper.
    #[test]
    fn maps_non_press_key_events_to_none() {
        let release = map_crossterm_event(CrosstermEvent::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
        let repeat = map_crossterm_event(CrosstermEvent::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));

        assert_eq!(release, None);
        assert_eq!(repeat, None);
    }

    /// Verifies non-matching event types such as mouse events are ignored.
    #[test]
    fn maps_non_matching_events_to_none() {
        let event = CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 20,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(map_crossterm_event(event), None);
    }

    /// Verifies mouse wheel events map to transcript scroll commands.
    #[test]
    fn mouse_wheel_events_map_to_scroll_commands() {
        let scroll_up = CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 20,
            modifiers: KeyModifiers::NONE,
        });
        let scroll_down = CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 20,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(map_crossterm_event(scroll_up), Some(TuiEvent::ScrollUp));
        assert_eq!(map_crossterm_event(scroll_down), Some(TuiEvent::ScrollDown));
    }

    /// Verifies resize events are mapped to [`TuiEvent::Resize`].
    #[test]
    fn resize_event_maps_to_tui_event_resize() {
        let event = map_crossterm_event(CrosstermEvent::Resize(120, 45));
        assert!(matches!(event, Some(TuiEvent::Resize)));
    }

    /// Verifies focus gain and loss events are both mapped to [`TuiEvent::Resize`].
    #[test]
    fn focus_gain_and_loss_maps_to_resize() {
        let gained = map_crossterm_event(CrosstermEvent::FocusGained);
        let lost = map_crossterm_event(CrosstermEvent::FocusLost);

        assert_eq!(gained, Some(TuiEvent::Resize));
        assert_eq!(lost, Some(TuiEvent::Resize));
    }

    /// Verifies paste events are converted and normalize CR to LF.
    #[test]
    fn paste_event_replaces_carriage_returns() {
        let event = map_crossterm_event(CrosstermEvent::Paste("line1\rline2\r".to_string()));
        assert_eq!(event, Some(TuiEvent::Paste("line1\nline2\n".to_string())));
    }
}
