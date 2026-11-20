//! Workspace commands and their key bindings.

use gpui_kit::{App, KeyBinding};

/// Key context of the workspace shell; every shell binding is scoped to it.
pub const WORKSPACE_CONTEXT: &str = "Workspace";
/// Key context around a session's message input.
pub const COMPOSER_CONTEXT: &str = "Composer";
/// Key context around the sidebar search field.
pub const SEARCH_CONTEXT: &str = "SidebarSearch";

gpui_kit::actions!(
    desktop,
    [
        /// Opens a draft session in the current workspace.
        NewSession,
        /// Closes the active tab.
        CloseTab,
        /// Activates the next tab, wrapping around.
        NextTab,
        /// Shows or hides the sidebar.
        ToggleSidebar,
        /// Opens and focuses the sidebar search field.
        FocusSearch,
        /// Closes the sidebar search field and clears its query.
        CloseSearch,
        /// Focuses the active tab's message input.
        FocusComposer,
        /// Stops the active run of the session whose composer has focus.
        StopRun,
        /// Opens the settings tab, or activates it when already open.
        OpenSettings,
        /// Quits the application.
        Quit
    ]
);

/// Activates the tab at a zero-based position.
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectTab {
    /// Zero-based tab position.
    pub index: usize,
}

/// Spells a shortcut with the platform's primary modifier: Cmd on macOS, Ctrl elsewhere.
macro_rules! primary {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

/// Shortcuts for the first nine tabs, in tab order.
const TAB_KEYS: [&str; 9] = [
    primary!("1"),
    primary!("2"),
    primary!("3"),
    primary!("4"),
    primary!("5"),
    primary!("6"),
    primary!("7"),
    primary!("8"),
    primary!("9"),
];

/// Registers the shell's shortcuts.
pub fn bind_keys(cx: &mut App) {
    let context = Some(WORKSPACE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new(primary!("n"), NewSession, context),
        KeyBinding::new(primary!("w"), CloseTab, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new(primary!("b"), ToggleSidebar, context),
        KeyBinding::new(primary!("k"), FocusSearch, context),
        KeyBinding::new(primary!("l"), FocusComposer, context),
        // The input handles Escape first and passes it on only when it has nothing to dismiss.
        KeyBinding::new("escape", StopRun, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("escape", CloseSearch, Some(SEARCH_CONTEXT)),
        KeyBinding::new(primary!(","), OpenSettings, context),
        KeyBinding::new(primary!("q"), Quit, context),
    ]);
    cx.bind_keys(TAB_KEYS.iter().enumerate().map(|(index, keys)| {
        KeyBinding::new(keys, SelectTab { index }, context)
    }));
}
