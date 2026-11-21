//! Shell lifecycle: starting the local backend, saving state, following the system appearance, quitting.

use std::path::PathBuf;
use std::time::Duration;

use futures::FutureExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::label::Label;
use gpui_kit::component::{ResizablePanelEvent, WindowExt as _};
use gpui_kit::{App, AppContext as _, Context, Window};
use protocol::ProductIdentity;

use crate::actions::Quit;
use crate::backend::{Backend, BackendError, BackendEvent};
use crate::ui::{AppView, BackendPhase};

/// Quiet period before a state change is written to `desktop.json`.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// Longest wait for the backend to flush and stop when quitting.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// The configuration file the backend reads, or the directory where it belongs.
///
/// Mirrors the config crate's search order: the override variable, the user
/// config file, then `claw.toml` in the working directory.
fn config_location() -> PathBuf {
    let user_dir = dirs::config_dir()
        .map(|dir| dir.join(ProductIdentity::CONFIG_DIR_NAME))
        .unwrap_or_default();
    let candidates = [
        std::env::var_os(ProductIdentity::CONFIG_PATH_ENV).map(PathBuf::from),
        Some(user_dir.join(ProductIdentity::USER_CONFIG_FILE_NAME)),
        Some(PathBuf::from(ProductIdentity::CONFIG_FILE_NAME)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.exists())
        .unwrap_or(user_dir)
}

impl AppView {
    /// Wires window-level observers: appearance, size, sidebar width, and the close button.
    pub(in crate::ui) fn observe_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let appearance =
            cx.observe_window_appearance(window, |this, window, cx| {
                this.state.appearance.apply(window.appearance(), cx);
            });
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            // Inner bounds leave out the client-side decoration shadow; the
            // outer bounds would grow the window by that inset on every launch.
            // A maximized or fullscreen window keeps the last windowed size.
            if let gpui_kit::WindowBounds::Windowed(bounds) =
                window.inner_window_bounds()
            {
                let size = bounds.size;
                this.state.layout.window_size =
                    Some((f32::from(size.width), f32::from(size.height)));
                this.persist(cx);
            }
        });
        let resize = cx.subscribe(
            &self.sidebar_split,
            |this, split, _: &ResizablePanelEvent, cx| {
                let sizes = split.read(cx).sizes();
                if let Some(width) = sizes.first() {
                    this.state.layout.sidebar_width = f32::from(*width);
                }
                // The third column is the panel while it shows beside the main card.
                let panel_beside = this
                    .panel()
                    .is_some_and(|panel| panel.open && !panel.maximized);
                if panel_beside && let Some(width) = sizes.get(2) {
                    this.state.layout.panel_width = f32::from(*width);
                }
                this.persist(cx);
            },
        );
        self._subscriptions.extend([appearance, bounds, resize]);
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            // Quitting stops the backend first; the window closes once that is done.
            let _ = view.update(cx, |this, cx| this.request_quit(window, cx));
            false
        });
    }

    /// Starts the local backend from a freshly loaded configuration; Retry
    /// uses it so fixes to the files take effect.
    pub fn start_backend(&mut self, cx: &mut Context<'_, Self>) {
        self.start_backend_with(config::load(), cx);
    }

    /// Starts the local backend from a configuration already loaded at launch.
    pub fn start_backend_with(
        &mut self,
        config: Result<config::ConfigHandle, config::ConfigError>,
        cx: &mut Context<'_, Self>,
    ) {
        self.host.phase = BackendPhase::Starting;
        cx.notify();
        let config = match config {
            Ok(config) => config,
            Err(error) => {
                self.set_backend(
                    Err(BackendError::Startup(error.to_string())),
                    cx,
                );
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = Backend::start(config).await;
            let _ =
                this.update(cx, |this, cx| this.set_local_backend(result, cx));
        })
        .detach();
    }

    /// Keeps the local backend for shutdown and connects the shell to it.
    fn set_local_backend(
        &mut self,
        result: Result<
            (Backend, UnboundedReceiver<BackendEvent>),
            BackendError,
        >,
        cx: &mut Context<'_, Self>,
    ) {
        match result {
            Ok((backend, events)) => {
                let client = backend.client().clone();
                self.host.backend = Some(backend);
                self.set_backend(Ok((client, events)), cx);
            }
            Err(error) => self.set_backend(Err(error), cx),
        }
    }

    /// Whether the backend connection ended while the app was running.
    pub fn backend_stopped(&self) -> bool {
        matches!(self.host.phase, BackendPhase::Stopped(_))
    }

    /// Opens the backend configuration in the system's default application.
    pub(in crate::ui) fn open_config(&self, cx: &mut Context<'_, Self>) {
        cx.open_with_system(&config_location());
    }

    /// Launches a fresh copy of the app and quits this one.
    pub(in crate::ui) fn restart_app(&mut self, cx: &mut Context<'_, Self>) {
        // The new process starts only after this one stopped its backend (spec §4.2).
        self.quit_now(true, cx);
    }

    /// Schedules a debounced save of the desktop state; a newer change replaces a pending save.
    pub(in crate::ui) fn schedule_save(&mut self, cx: &mut Context<'_, Self>) {
        let state = self.state.clone();
        let path = self.state_path.clone();
        self._save_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let written =
                cx.background_spawn(async move { state.save(&path) }).await;
            if let Err(error) = written {
                tracing::warn!("failed to save desktop state: {}", error);
            }
        }));
    }

    /// Writes the desktop state immediately, cancelling a pending debounced save.
    fn save_now(&mut self) {
        self._save_task = None;
        if let Err(error) = self.state.save(&self.state_path) {
            tracing::warn!("failed to save desktop state: {}", error);
        }
    }

    /// `Quit`: asks first when sessions are running.
    pub(in crate::ui) fn on_quit(
        &mut self,
        _: &Quit,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.request_quit(window, cx);
    }

    /// Quits now, or asks for confirmation while runs are active.
    fn request_quit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.quitting {
            return;
        }
        let running = self
            .host
            .registry
            .as_ref()
            .map_or(0, |registry| registry.read(cx).running_count(cx));
        if running == 0 {
            self.quit_now(false, cx);
            return;
        }
        let title = if running == 1 {
            rust_i18n::t!("Quit.title_one").into_owned()
        } else {
            rust_i18n::t!("Quit.title_other", count = running).into_owned()
        };
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .confirm()
                .title(Label::new(title.clone()))
                .description(Label::new(
                    rust_i18n::t!("Quit.body").into_owned(),
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(rust_i18n::t!("Quit.action").into_owned())
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(
                            rust_i18n::t!("Dialog.cancel").into_owned(),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ =
                        view.update(cx, |this, cx| this.quit_now(false, cx));
                    true
                })
        });
    }

    /// Saves state, stops the backend within the timeout, optionally relaunches, then quits.
    fn quit_now(&mut self, restart: bool, cx: &mut Context<'_, Self>) {
        self.quitting = true;
        self.save_now();
        let backend = self.host.backend.take();
        tracing::info!("quitting desktop app");
        cx.spawn(async move |_, cx| {
            if let Some(backend) = backend {
                let timer = cx.background_executor().timer(SHUTDOWN_TIMEOUT);
                futures::select! {
                    () = backend.shutdown().fuse() => {}
                    () = timer.fuse() => tracing::warn!("backend did not stop within {:?}", SHUTDOWN_TIMEOUT),
                }
            }
            if restart {
                let spawned = std::env::current_exe()
                    .and_then(|path| std::process::Command::new(path).spawn());
                if let Err(error) = spawned {
                    tracing::error!("failed to restart the app: {}", error);
                }
            }
            cx.update(|cx: &mut App| cx.quit());
        })
        .detach();
    }
}
