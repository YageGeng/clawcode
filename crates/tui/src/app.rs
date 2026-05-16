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

use crate::acp_client::{self, AcpClient, AppEvent};
use crate::event::{TuiEvent, map_crossterm_event};
use crate::terminal::enter;
use crate::ui::approval::decision_for_key;
use crate::ui::composer::{Composer, ComposerAction};
use crate::ui::render::render;
use crate::ui::state::AppState;
use crate::ui::view::ViewState;

type TuiTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

/// Number of transcript rows moved for one wheel-equivalent scroll input.
const SCROLL_LINES: u16 = 3;

/// Runs a full interactive loop for one ACP session.
pub async fn run(
    cwd: PathBuf,
    resume: Option<SessionId>,
    use_alt_screen: bool,
) -> anyhow::Result<()> {
    let (app_tx, app_rx) = mpsc::unbounded_channel::<AppEvent>();
    acp_client::with_in_process_client(app_tx.clone(), move |client| async move {
        client.initialize().await?;
        let (session_id, model_label) = open_session(&client, cwd.clone(), resume).await?;
        let mut state = AppState::new(session_id, cwd, model_label);
        let mut view = ViewState::default();
        let mut composer = Composer::default();

        let (mut terminal, mut terminal_guard) = enter(use_alt_screen)?;
        let mut ui = UiRuntime {
            state: &mut state,
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
        let response = client.list_sessions(cwd).await?;
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
    terminal.draw(|frame| render(frame, ui.state, ui.view, ui.composer.text()))?;

    let mut terminal_events = EventStream::new().fuse();
    let mut redraw = time::interval(Duration::from_millis(100));
    let mut prompt_task: Option<JoinHandle<()>> = None;

    loop {
        let mut should_exit = false;
        select! {
            app_event = app_rx.recv() => {
                if let Some(event) = app_event {
                    handle_app_event(ui.state, event, &mut prompt_task).await;
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
                                    )?
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
                        ui.state.set_error(format!("terminal input error: {error}"));
                    }
                    None => should_exit = true,
                }
            }
            _ = redraw.tick() => {}
        }

        if should_exit {
            break;
        }
        terminal.draw(|frame| render(frame, ui.state, ui.view, ui.composer.text()))?;
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
    state: &mut AppState,
    event: AppEvent,
    prompt_task: &mut Option<JoinHandle<()>>,
) {
    match event {
        AppEvent::SessionNotification(notification) => {
            state.apply_session_update(*notification);
        }
        AppEvent::PermissionRequested(approval) => {
            state.set_pending_approval(approval);
        }
        AppEvent::PromptFinished(stop_reason) => {
            state.finish_prompt(stop_reason);
            if let Some(handle) = prompt_task.take() {
                let _ = handle.await;
            }
        }
        AppEvent::PromptFailed(message) | AppEvent::AcpError(message) => {
            state.set_error(message);
            if let Some(handle) = prompt_task.take() {
                let _ = handle.await;
            }
        }
    }
}

/// Returns true when the key event should exit the loop.
fn handle_key_event(
    client: &AcpClient,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
    ui: &mut UiRuntime<'_>,
    key_event: KeyEvent,
    prompt_task: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    if let Some(approval) = ui.state.pending_approval() {
        let request_id = approval.request_id();
        return handle_approval_key(client, ui.state, request_id, key_event);
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
        KeyCode::Home => {
            ui.view.scroll_top();
            Ok(false)
        }
        KeyCode::End => {
            ui.view.follow_bottom();
            Ok(false)
        }
        KeyCode::Char('c') if key_event.modifiers == KeyModifiers::CONTROL => {
            if ui.state.is_running_prompt() {
                if let Err(error) = client.cancel(ui.state.session_id().clone()) {
                    ui.state.set_error(error.to_string());
                }
            } else {
                return Ok(true);
            }
            Ok(false)
        }
        KeyCode::Esc if !ui.state.is_running_prompt() => Ok(true),
        _ => {
            let action = ui.composer.handle_key(key_event);
            if let ComposerAction::Submit(text) = action
                && !text.trim().is_empty()
            {
                run_prompt(client, app_tx, ui.state, text, prompt_task);
            }
            Ok(false)
        }
    }
}

/// Mutable UI pieces shared while handling one terminal key event.
struct UiRuntime<'a> {
    /// Renderable ACP event state.
    state: &'a mut AppState,
    /// View-only scroll and folding state.
    view: &'a mut ViewState,
    /// Editable prompt composer state.
    composer: &'a mut Composer,
}

/// Handles approval decisions and returns whether the app should exit.
fn handle_approval_key(
    client: &AcpClient,
    state: &mut AppState,
    request_id: u64,
    key_event: KeyEvent,
) -> anyhow::Result<bool> {
    if let Some(decision) = decision_for_key(key_event) {
        if let Err(error) = client.resolve_permission(request_id, decision) {
            state.set_error(format!("failed to resolve approval: {error}"));
            return Ok(false);
        }

        state.clear_pending_approval();
    }

    Ok(false)
}

/// Starts one ACP prompt and streams ACP notifications back into AppState.
fn run_prompt(
    client: &AcpClient,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    submitted: String,
    prompt_task: &mut Option<JoinHandle<()>>,
) {
    if state.is_running_prompt() || prompt_task.is_some() {
        return;
    }

    state.append_user_message(&submitted);
    let session_id = state.session_id().clone();
    let tx = app_tx.clone();
    let client = client.clone();

    let handle = tokio::spawn(async move {
        match client.prompt(session_id, submitted).await {
            Ok(stop_reason) => {
                let _ = tx.send(AppEvent::PromptFinished(stop_reason));
            }
            Err(error) => {
                let _ = tx.send(AppEvent::PromptFailed(error.to_string()));
            }
        }
    });

    *prompt_task = Some(handle);
}
