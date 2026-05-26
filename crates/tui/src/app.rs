//! Local TUI session loop and ACP event processing.

use std::path::PathBuf;

use agent_client_protocol::schema::{
    ListSessionsResponse, LoadSessionResponse, NewSessionResponse, SessionId,
};
use crossterm::event::{EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio::{select, task::JoinHandle};

use crate::acp::client::{self as acp_client, AcpClient, AppEvent};
use crate::event::{TuiEvent, map_crossterm_event};
use crate::terminal::enter;
use crate::ui::approval::decision_for_key;
use crate::ui::composer::{Composer, ComposerAction};
use crate::ui::render::render_router;
use crate::ui::session_router::SessionRouterState;
use crate::ui::theme::Theme;
use crate::ui::view::ViewState;
use kernel::command::slash_command::SlashCommand;

type TuiTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

/// Number of transcript rows moved for one wheel-equivalent scroll input.
const SCROLL_LINES: u16 = 3;

/// Runs a full interactive loop for one ACP session.
pub async fn run(
    cwd: PathBuf,
    resume: Option<SessionId>,
    use_alt_screen: bool,
) -> anyhow::Result<()> {
    let theme = Theme::from_config(config::load()?.current().tui.theme);
    let (app_tx, app_rx) = mpsc::unbounded_channel::<AppEvent>();
    acp_client::with_in_process_client(app_tx.clone(), move |client| async move {
        client.initialize().await?;
        let (session_id, model_label) = open_session(&client, cwd.clone(), resume).await?;
        let mut router = SessionRouterState::new(session_id, cwd, model_label, theme);
        let mut view = ViewState::default();
        let mut composer = Composer::default();

        let (mut terminal, mut terminal_guard) = enter(use_alt_screen)?;
        let mut ui = UiRuntime {
            router: &mut router,
            view: &mut view,
            composer: &mut composer,
        };
        let result = run_loop(client, app_rx, app_tx, &mut terminal, &mut ui).await;

        let restore_result = terminal_guard.restore();
        match (result, restore_result) {
            (Err(run_error), Err(restore_error)) => {
                tracing::warn!(
                    restore_error = %restore_error,
                    "failed to restore terminal after run failure"
                );
                Err(run_error)
            }
            (Ok(()), Err(restore_error)) => Err(restore_error),
            (result, Ok(())) => result,
        }
    })
    .await
}

/// Lists persisted sessions through the in-process ACP server.
pub async fn list_sessions(cwd: PathBuf) -> anyhow::Result<()> {
    let (app_tx, _app_rx) = mpsc::unbounded_channel::<AppEvent>();
    acp_client::with_in_process_client(app_tx, move |client| async move {
        client.initialize().await?;
        let response = client.list_sessions(cwd, None).await?;
        print_sessions(response);
        Ok(())
    })
    .await
}

/// Opens a new or persisted ACP session and returns its session id plus model label.
async fn open_session(
    client: &AcpClient,
    cwd: PathBuf,
    resume: Option<SessionId>,
) -> anyhow::Result<(SessionId, String)> {
    if let Some(session_id) = resume {
        let response = client.load_session(session_id.clone(), cwd).await?;
        eprintln!("resumed session: {session_id}");
        return Ok((session_id, model_label_from_load(&response)));
    }

    let response = client.new_session(cwd).await?;
    eprintln!("session: {}", response.session_id);
    let model_label = model_label_from_new(&response);
    Ok((response.session_id, model_label))
}

/// Returns a compact model label from a new-session response.
fn model_label_from_new(response: &NewSessionResponse) -> String {
    response
        .models
        .as_ref()
        .map(|models| models.current_model_id.0.to_string())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| "model: unknown".to_string())
}

/// Returns a compact model label from a load-session response.
fn model_label_from_load(response: &LoadSessionResponse) -> String {
    response
        .models
        .as_ref()
        .map(|models| models.current_model_id.0.to_string())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| "model: unknown".to_string())
}

/// Prints persisted session metadata for `cwd` and exits.
fn print_sessions(response: ListSessionsResponse) {
    if response.sessions.is_empty() {
        println!("No sessions found for this working directory.");
        return;
    }

    for session in response.sessions {
        let updated = session.updated_at.as_deref().unwrap_or("-");
        let title = session.title.as_deref().unwrap_or("-");
        println!(
            "{}\t{}\t{}\t{}",
            session.session_id,
            updated,
            session.cwd.display(),
            title
        );
    }

    if let Some(next) = response.next_cursor {
        println!();
        println!("next cursor: {next}");
    }
}

/// Runs the main draw/event loop with a shared render state.
async fn run_loop(
    client: AcpClient,
    mut app_rx: mpsc::UnboundedReceiver<AppEvent>,
    app_tx: mpsc::UnboundedSender<AppEvent>,
    terminal: &mut TuiTerminal,
    ui: &mut UiRuntime<'_>,
) -> anyhow::Result<()> {
    terminal.draw(|frame| render_router(frame, ui.router, ui.view, ui.composer))?;

    let mut terminal_events = EventStream::new().fuse();
    let mut redraw = time::interval(Duration::from_millis(100));
    let mut prompt_task: Option<JoinHandle<()>> = None;

    loop {
        let mut should_exit = false;
        select! {
            app_event = app_rx.recv() => {
                if let Some(event) = app_event {
                    handle_app_event(ui.router, event, &mut prompt_task).await;
                }
            }
            term_event = terminal_events.next() => {
                match term_event {
                    Some(Ok(raw_event)) => {
                        if let Some(event) = map_crossterm_event(raw_event) {
                            should_exit = match event {
                                TuiEvent::Key(key_event) => {
                                    handle_key_event(
                                        &client,
                                        &app_tx,
                                        ui,
                                        key_event,
                                        &mut prompt_task,
                                    ).await?
                                }
                                TuiEvent::Paste(text) => {
                                    ui.composer.insert_str(&text);
                                    false
                                }
                                TuiEvent::ScrollUp => {
                                    ui.view.scroll_page_up(SCROLL_LINES);
                                    false
                                }
                                TuiEvent::ScrollDown => {
                                    ui.view.scroll_page_down(SCROLL_LINES);
                                    false
                                }
                                TuiEvent::Resize | TuiEvent::Tick => false,
                            };
                        }
                    }
                    Some(Err(error)) => {
                        ui.router
                            .active_state_mut()
                            .set_error(format!("terminal input error: {error}"));
                    }
                    None => should_exit = true,
                }
            }
            _ = redraw.tick() => {}
        }

        if should_exit {
            break;
        }
        terminal.draw(|frame| render_router(frame, ui.router, ui.view, ui.composer))?;
    }

    client.reject_pending_permissions();
    if let Some(handle) = prompt_task.take() {
        handle.abort();
        let _ = handle.await;
    }

    Ok(())
}

/// Applies one ACP app event to renderable state.
async fn handle_app_event(
    router: &mut SessionRouterState,
    event: AppEvent,
    prompt_task: &mut Option<JoinHandle<()>>,
) {
    match event {
        AppEvent::SessionNotification(notification) => {
            router.apply_session_notification(*notification);
        }
        AppEvent::PermissionRequested(approval) => {
            router.active_state_mut().set_pending_approval(approval);
        }
        AppEvent::PromptFinished {
            session_id,
            stop_reason,
        } => {
            router.finish_prompt_for_session(&session_id, stop_reason);
            if let Some(handle) = prompt_task.take() {
                let _ = handle.await;
            }
        }
        AppEvent::PromptFailed {
            session_id,
            message,
        } => {
            router.set_error_for_session(&session_id, message);
            if let Some(handle) = prompt_task.take() {
                let _ = handle.await;
            }
        }
        AppEvent::AcpError(message) => {
            router.active_state_mut().set_error(message);
        }
    }
}

/// Returns true when the key event should exit the loop.
async fn handle_key_event(
    client: &AcpClient,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
    ui: &mut UiRuntime<'_>,
    key_event: KeyEvent,
    prompt_task: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    if let Some(approval) = ui.router.active_state().pending_approval() {
        let request_id = approval.request_id();
        return handle_approval_key(client, ui.router, request_id, key_event);
    }

    if ui.router.is_agent_picker_focused() {
        if let Some(target_session_id) = handle_agent_picker_key(ui.router, key_event.code)? {
            if !ensure_loaded_agent_session(client, ui.router, &target_session_id).await {
                return Ok(false);
            }
            ui.router
                .select_agent_session(target_session_id, ui.view, ui.composer)?;
            ui.router.close_agent_picker();
        }
        return Ok(false);
    }

    match key_event.code {
        KeyCode::PageUp => {
            ui.view.scroll_page_up(10);
            Ok(false)
        }
        KeyCode::PageDown => {
            ui.view.scroll_page_down(10);
            Ok(false)
        }
        KeyCode::Up => {
            ui.view.scroll_page_up(SCROLL_LINES);
            Ok(false)
        }
        KeyCode::Down => {
            ui.view.scroll_page_down(SCROLL_LINES);
            Ok(false)
        }
        KeyCode::Home if key_event.modifiers == KeyModifiers::CONTROL => {
            ui.view.scroll_top();
            Ok(false)
        }
        KeyCode::End if key_event.modifiers == KeyModifiers::CONTROL => {
            ui.view.follow_bottom();
            Ok(false)
        }
        KeyCode::Char('c') if key_event.modifiers == KeyModifiers::CONTROL => {
            if ui.router.active_state().is_running_prompt() {
                if let Err(error) = client.cancel(ui.router.active_session_id().clone()) {
                    ui.router.active_state_mut().set_error(error.to_string());
                }
            } else {
                return Ok(true);
            }
            Ok(false)
        }
        KeyCode::Esc if !ui.router.active_state().is_running_prompt() => Ok(true),
        _ => {
            let action = ui.composer.handle_key(key_event);
            if let ComposerAction::Submit(text) = action
                && !text.trim().is_empty()
            {
                if handle_local_command(ui, &text) {
                    return Ok(false);
                }
                run_prompt(client, app_tx, ui.router, text, prompt_task);
            }
            Ok(false)
        }
    }
}

/// Try to load a target agent session before switching to it.
async fn ensure_loaded_agent_session(
    client: &AcpClient,
    router: &mut SessionRouterState,
    target_session_id: &SessionId,
) -> bool {
    if router.has_state(target_session_id) {
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
    true
}

/// Mutable UI pieces shared while handling one terminal key event.
struct UiRuntime<'a> {
    /// Renderable ACP event router state.
    router: &'a mut SessionRouterState,
    /// View-only scroll and folding state.
    view: &'a mut ViewState,
    /// Editable prompt composer state.
    composer: &'a mut Composer,
}

/// Handles approval decisions and returns whether the app should exit.
fn handle_approval_key(
    client: &AcpClient,
    router: &mut SessionRouterState,
    request_id: u64,
    key_event: KeyEvent,
) -> anyhow::Result<bool> {
    if let Some(decision) = decision_for_key(key_event) {
        if let Err(error) = client.resolve_permission(request_id, decision) {
            router
                .active_state_mut()
                .set_error(format!("failed to resolve approval: {error}"));
            return Ok(false);
        }

        router.active_state_mut().clear_pending_approval();
    }

    Ok(false)
}

/// Handles local TUI slash commands before they reach the ACP prompt path.
fn handle_local_command(ui: &mut UiRuntime<'_>, text: &str) -> bool {
    match SlashCommand::parse_from_text(text) {
        Some(SlashCommand::Raw) => handle_raw_command(ui, text),
        Some(SlashCommand::Agent) => {
            ui.router.open_agent_picker();
            true
        }
        _ => false,
    }
}

/// Handles `/raw` transcript mode commands before they reach the ACP prompt path.
fn handle_raw_command(ui: &mut UiRuntime<'_>, text: &str) -> bool {
    let trimmed = text.trim();
    let explicit = parse_raw_arg(trimmed);
    let Some(explicit) = explicit else {
        ui.router
            .active_state_mut()
            .add_system_message("Usage: /raw [on|off]");
        return true;
    };
    let enabled = explicit.unwrap_or_else(|| ui.view.toggle_raw_output_mode());
    ui.view.set_raw_output_mode(enabled);
    ui.router
        .active_state_mut()
        .add_system_message(raw_output_mode_notice(enabled));
    true
}

/// Handles keys while the inline agent picker has focus.
fn handle_agent_picker_key(
    router: &mut SessionRouterState,
    code: KeyCode,
) -> anyhow::Result<Option<SessionId>> {
    match code {
        KeyCode::Up => {
            router.move_agent_picker_previous();
            Ok(None)
        }
        KeyCode::Down => {
            router.move_agent_picker_next();
            Ok(None)
        }
        KeyCode::Enter => Ok(router.selected_agent_session_id().cloned()),
        KeyCode::Esc => {
            router.close_agent_picker();
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Parses `/raw`, `/raw on`, and `/raw off` argument.
fn parse_raw_arg(text: &str) -> Option<Option<bool>> {
    let mut parts = text.split_whitespace();
    if parts.next()? != "/raw" {
        return None;
    }
    match (parts.next(), parts.next()) {
        (None, None) => Some(None),
        (Some("on"), None) => Some(Some(true)),
        (Some("off"), None) => Some(Some(false)),
        _ => None,
    }
}

/// Returns the user-visible raw output mode notice.
fn raw_output_mode_notice(enabled: bool) -> &'static str {
    if enabled {
        "Raw output mode on: transcript text is shown for clean terminal selection."
    } else {
        "Raw output mode off: rich transcript rendering restored."
    }
}
/// Starts one ACP prompt and streams ACP notifications back into AppState.
fn run_prompt(
    client: &AcpClient,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
    router: &mut SessionRouterState,
    submitted: String,
    prompt_task: &mut Option<JoinHandle<()>>,
) {
    if router.active_state().is_running_prompt() || prompt_task.is_some() {
        return;
    }

    router.active_state_mut().append_user_message(&submitted);
    let session_id = router.active_session_id().clone();
    let tx = app_tx.clone();
    let client = client.clone();
    let prompt_session_id = session_id.clone();

    let handle = tokio::spawn(async move {
        match client.prompt(session_id.clone(), submitted).await {
            Ok(stop_reason) => {
                let _ = tx.send(AppEvent::PromptFinished {
                    session_id: prompt_session_id,
                    stop_reason,
                });
            }
            Err(error) => {
                let _ = tx.send(AppEvent::PromptFailed {
                    session_id,
                    message: error.to_string(),
                });
            }
        }
    });

    *prompt_task = Some(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::agent_navigation::{AgentPickerEntry, AgentPickerStatus};

    /// Build a test UI runtime backed by a session router.
    fn test_ui_runtime<'a>(
        router: &'a mut SessionRouterState,
        view: &'a mut ViewState,
        composer: &'a mut Composer,
    ) -> UiRuntime<'a> {
        UiRuntime {
            router,
            view,
            composer,
        }
    }

    /// Build a router for local command tests.
    fn test_router() -> SessionRouterState {
        SessionRouterState::new(
            agent_client_protocol::schema::SessionId::new("s1".to_string()),
            std::path::PathBuf::from("."),
            "provider/model".to_string(),
            Theme::dark(),
        )
    }

    /// Verifies slash command parsing via handle_raw_command.
    #[test]
    fn raw_command_parses_toggle_and_explicit_modes() {
        let mut router = test_router();
        let mut view = ViewState::default();
        let mut composer = Composer::default();
        let mut ui = test_ui_runtime(&mut router, &mut view, &mut composer);

        // toggle
        assert!(handle_raw_command(&mut ui, "/raw"));
        assert!(ui.view.raw_output_mode());

        // explicit on
        assert!(handle_raw_command(&mut ui, "/raw on"));
        assert!(ui.view.raw_output_mode());

        // explicit off
        assert!(handle_raw_command(&mut ui, "/raw off"));
        assert!(!ui.view.raw_output_mode());
        // invalid arg
        assert!(handle_raw_command(&mut ui, "/raw nope"));
    }
    /// Verifies raw command handling mutates only local TUI state.
    #[test]
    fn raw_command_toggles_view_state_and_adds_notice() {
        let mut router = test_router();
        let mut view = ViewState::default();
        let mut composer = Composer::default();
        let mut ui = test_ui_runtime(&mut router, &mut view, &mut composer);

        assert!(handle_raw_command(&mut ui, "/raw"));

        assert!(ui.view.raw_output_mode());
        assert!(ui.router.active_state().transcript().iter().any(|entry| {
            entry
                .text_cell()
                .is_some_and(|cell| cell.text().contains("Raw output mode on"))
        }));
    }

    /// Verifies `/agent` opens the inline picker locally.
    #[test]
    fn agent_command_opens_inline_picker() {
        let mut router = test_router();
        let mut view = ViewState::default();
        let mut composer = Composer::default();
        let mut ui = test_ui_runtime(&mut router, &mut view, &mut composer);

        assert!(handle_local_command(&mut ui, "/agent"));

        assert!(ui.router.is_agent_picker_focused());
    }

    /// Verifies focused picker consumes Up/Down/Enter and selects the expected session.
    #[test]
    fn focused_agent_picker_handles_arrow_and_enter() {
        let root = SessionId::new("root-session");
        let child = SessionId::new("child-session");
        let mut router = SessionRouterState::new(
            root.clone(),
            "/tmp/project".into(),
            "provider/model".to_string(),
            Theme::dark(),
        );
        router.agent_navigation_mut().upsert(
            AgentPickerEntry::builder()
                .session_id(child.clone())
                .parent_session_id(root)
                .agent_path("/root/inspect".to_string())
                .status(AgentPickerStatus::Running)
                .is_root(false)
                .build(),
        );
        router.open_agent_picker();

        assert!(
            handle_agent_picker_key(&mut router, KeyCode::Down)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            handle_agent_picker_key(&mut router, KeyCode::Enter).unwrap(),
            Some(child)
        );
    }

    /// Verifies non-command text that merely shares the prefix is not consumed.
    #[test]
    fn raw_command_does_not_consume_prefix_matches() {
        assert_eq!(SlashCommand::parse_from_text("/rawhide"), None);
        assert_eq!(SlashCommand::parse_from_text(" /raw"), None);
    }
}
