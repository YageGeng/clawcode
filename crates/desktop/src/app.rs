//! Application bootstrap: logging, saved state, gpui-kit, the main window, and the backend.

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AppContext as _, WindowBounds, WindowDecorations, WindowOptions, px, size,
};

use crate::assets::AppAssets;
use crate::state::DesktopState;
use crate::ui::AppView;

/// Smallest window that keeps the sidebar and card usable.
const MIN_WINDOW: (f32, f32) = (960.0, 640.0);
/// Window size on first launch.
const DEFAULT_WINDOW: (f32, f32) = (1280.0, 820.0);

/// Installs process-wide logging from the backend configuration, falling back to `info`.
fn install_logging(config: Option<&config::ConfigHandle>) {
    let factory = match config {
        Some(config) => {
            app::LoggingFactory::from_config(&config.current().logging)
        }
        // The window explains a broken config; logging still has to start.
        None => app::LoggingFactory::new("info", false),
    };
    if let Err(error) = factory.and_then(app::LoggingFactory::install) {
        // Nothing else can report this: the subscriber is what failed.
        eprintln!("failed to install logging: {error}");
    }
}

/// Runs the desktop application until it quits.
pub fn run() {
    // Read once: logging and the backend must agree on the same configuration.
    let config = config::load();
    install_logging(config.as_ref().ok());
    let state_path = DesktopState::default_path().unwrap_or_else(|| {
        tracing::warn!("no platform config directory; keeping desktop state in the working directory");
        PathBuf::from("desktop.json")
    });
    let state = DesktopState::load(&state_path);
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            crate::theme::register_builtin_themes(cx);
            state.language.apply(cx);
            crate::actions::bind_keys(cx);
            let (width, height) = state
                .layout
                .window_size
                .map(|(width, height)| {
                    (width.max(MIN_WINDOW.0), height.max(MIN_WINDOW.1))
                })
                .unwrap_or(DEFAULT_WINDOW);
            let options = WindowOptions {
                window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
                window_bounds: Some(WindowBounds::centered(
                    size(px(width), px(height)),
                    cx,
                )),
                // Linux: ask for client-side decorations so the title bar draws the window controls
                // instead of a second, system title bar; compositors that refuse keep their own.
                window_decorations: cfg!(target_os = "linux")
                    .then_some(WindowDecorations::Client),
                ..TitleBar::window_options()
            };
            let opened =
                gpui_kit::open_window(options, cx, move |window, cx| {
                    state.appearance.apply(window.appearance(), cx);
                    cx.new(|cx| AppView::new(state, state_path, window, cx))
                });
            match opened {
                Ok((_, view)) => view
                    .update(cx, |view, cx| view.start_backend_with(config, cx)),
                Err(error) => {
                    tracing::error!("failed to open the main window: {}", error)
                }
            }
        });
}
