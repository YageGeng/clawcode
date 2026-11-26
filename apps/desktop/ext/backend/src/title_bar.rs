//! Styling for the system buttons over the app-drawn title bar.

use nsh_core::prelude::*;

/// Appearance of the native window controls in the desktop shell.
#[module]
pub struct WindowControls;

#[module]
impl WindowControls {
    /// Applies the title bar stylesheet once per GTK process, including across app hot reloads.
    pub fn style_title_bar() {
        #[cfg(target_os = "linux")]
        if gtk4::is_initialized() {
            static INSTALL: std::sync::Once = std::sync::Once::new();
            INSTALL.call_once(|| {
                // NSH calls native modules on its engine thread. GTK widgets belong to the UI loop.
                gtk4::glib::idle_add_once(|| {
                    if let Some(display) = gtk4::gdk::Display::default() {
                        let css = gtk4::CssProvider::new();
                        css.load_from_string(include_str!("title_bar.css"));
                        gtk4::style_context_add_provider_for_display(
                            &display,
                            &css,
                            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
                        );
                    }
                });
            });
        }
    }
}
